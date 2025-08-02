use std::error::Error as StdError;
use std::fs::File;
use std::io::Read;

use rayon::prelude::*;
use rayon::slice::ParallelSliceMut;

#[derive(Debug)]
pub struct MidiHeader {
    pub format: u16,
    pub tracks: u16,
    pub ppqn: u16,
}

#[derive(Debug, Clone)]
pub struct MidiEvent {
    pub absolute_ns: u64,
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
    pub track_index: u16,
    pub sysex_data: Option<Vec<u8>>,
}

pub struct MidiParser {
    is_parsed: bool,
    header: MidiHeader,
    pub events: Vec<MidiEvent>,
}

#[derive(Debug)]
enum TempEventData {
    Midi { status: u8, data1: u8, data2: u8 },
    TempoChange { new_tempo_us: u32 },
    SysEx { data: Vec<u8> },
}

#[derive(Debug)]
struct TempEvent {
    absolute_tick: u64,
    track_index: u16,
    data: TempEventData,
}

impl MidiParser {
    pub fn new() -> MidiParser {
        MidiParser {
            is_parsed: false,
            header: MidiHeader {
                format: 0,
                tracks: 0,
                ppqn: 0,
            },
            events: Vec::new(),
        }
    }

    pub fn get_header(&self) -> Option<&MidiHeader> {
        if self.is_parsed {
            Some(&self.header)
        } else {
            None
        }
    }

    fn tempo_to_tick_ns(tempo_us: u32, ppqn: u16) -> u64 {
        (tempo_us as u64 * 1000) / ppqn as u64
    }

    fn parse_track(
        track_index: u16,
        track_data: &[u8],
        total_tracks: u16,
    ) -> Result<Vec<TempEvent>, Box<dyn StdError + Send + Sync>> {
        let mut track_events = Vec::new();
        let mut index = 0;
        let mut last_status: Option<u8> = None;
        let mut absolute_tick = 0u64;

        while index < track_data.len() {
            // Read delta time using VLQ
            let mut delta_ticks = 0u32;
            loop {
                if index >= track_data.len() {
                    break;
                }
                let byte = track_data[index];
                index += 1;
                delta_ticks = (delta_ticks << 7) | (byte & 0x7F) as u32;
                if byte & 0x80 == 0 {
                    break;
                }
            }
            absolute_tick += delta_ticks as u64;

            if index >= track_data.len() {
                break;
            }

            // Status byte and running status
            let mut status = track_data[index];
            if status & 0x80 != 0 {
                index += 1;
                last_status = Some(status);
            } else if let Some(last) = last_status {
                status = last;
            } else {
                return Err(format!(
                    "Running status without previous status on track {}",
                    track_index
                )
                .into());
            }

            if status == 0xFF {
                // Meta Event
                if index + 1 > track_data.len() {
                    break;
                }
                let meta_type = track_data[index];
                index += 1;

                let mut length = 0usize;
                loop {
                    if index >= track_data.len() {
                        break;
                    }
                    let byte = track_data[index];
                    index += 1;
                    length = (length << 7) | (byte & 0x7F) as usize;
                    if byte & 0x80 == 0 {
                        break;
                    }
                }

                if index + length > track_data.len() {
                    break;
                }

                match meta_type {
                    0x51 if length == 3 => {
                        // Tempo change
                        let new_tempo_us = ((track_data[index] as u32) << 16)
                            | ((track_data[index + 1] as u32) << 8)
                            | (track_data[index + 2] as u32);
                        track_events.push(TempEvent {
                            absolute_tick,
                            track_index,
                            data: TempEventData::TempoChange { new_tempo_us },
                        });
                    }
                    0x2F if length == 0 => {
                        // End of track
                        break;
                    }
                    _ => { /* Ignore other meta event */ }
                }
                index += length;
            } else if status == 0xF0 {
                // System Exclusive (SysEx) message
                let mut sysex_data = Vec::new();

                while index < track_data.len() {
                    let byte = track_data[index];
                    index += 1;
                    sysex_data.push(byte);
                    if byte == 0xF7 {
                        break; // End of SysEx
                    }
                }

                track_events.push(TempEvent {
                    absolute_tick,
                    track_index,
                    data: TempEventData::SysEx { data: sysex_data },
                });
            } else if status & 0xF0 != 0xF0 {
                // MIDI channel message
                if index >= track_data.len() {
                    break;
                }
                let data1 = track_data[index];
                index += 1;

                let data2 = if status & 0xF0 != 0xC0 && status & 0xF0 != 0xD0 {
                    if index >= track_data.len() {
                        break;
                    }
                    let d = track_data[index];
                    index += 1;
                    d
                } else {
                    0
                };

                track_events.push(TempEvent {
                    absolute_tick,
                    track_index,
                    data: TempEventData::Midi {
                        status,
                        data1,
                        data2,
                    },
                });
            } else {
                if index < track_data.len() {
                    index += 1;
                }
            }
        }

        let thread_id_str = match rayon::current_thread_index() {
            Some(id) => id.to_string(),
            None => "N/A".to_string(),
        };

        println!(
            "[Thread {}] Track {:>2}/{} parsed ({} bytes), collected {} temp events",
            thread_id_str,
            track_index + 1,
            total_tracks,
            track_data.len(),
            track_events.len()
        );

        Ok(track_events)
    }

    pub fn parse_file(&mut self, file_path: &str) -> Result<(), Box<dyn StdError>> {
        let mut file = File::open(file_path)?;
        let mut buffer32 = [0; 4];

        file.read_exact(&mut buffer32)?;
        if buffer32 != *b"MThd" {
            return Err("Invalid header: no MThd".into());
        }

        file.read_exact(&mut buffer32)?;
        let header_length = u32::from_be_bytes(buffer32);
        if header_length != 6 {
            return Err(format!("Unexpected MThd chunk length: {}", header_length).into());
        }

        let mut header_data = [0; 6];
        file.read_exact(&mut header_data)?;

        self.header = MidiHeader {
            format: u16::from_be_bytes([header_data[0], header_data[1]]),
            tracks: u16::from_be_bytes([header_data[2], header_data[3]]),
            ppqn: u16::from_be_bytes([header_data[4], header_data[5]]),
        };

        let mut all_track_data = Vec::with_capacity(self.header.tracks as usize);
        for i in 0..self.header.tracks {
            file.read_exact(&mut buffer32)?;
            if buffer32 != *b"MTrk" {
                return Err(format!("Expected MTrk, found {:?} at track {}", buffer32, i).into());
            }

            file.read_exact(&mut buffer32)?;
            let track_length = u32::from_be_bytes(buffer32);
            let mut track_data = vec![0; track_length as usize];
            file.read_exact(&mut track_data)?;
            all_track_data.push(track_data);
        }

        println!("[KazuMIDIParser] Parsing {} tracks...", self.header.tracks);
        let parsing_results: Vec<Result<Vec<TempEvent>, _>> = all_track_data
            .into_par_iter()
            .enumerate()
            .map(|(i, data)| Self::parse_track(i as u16, &data, self.header.tracks))
            .collect();

        let mut temp_events: Vec<TempEvent> = Vec::new();
        for result in parsing_results {
            match result {
                Ok(track_events) => {
                    temp_events.extend(track_events);
                }
                Err(e) => {
                    return Err(e.to_string().into());
                }
            }
        }

        println!(
            "[KazuMIDIParser] All tracks parsed, total {} temp events collected.",
            temp_events.len()
        );

        println!("[KazuMIDIParser] Sorting merged events...");
        temp_events.par_sort_by_key(|e| e.absolute_tick);

        self.events.clear();

        println!("[KazuMIDIParser] Pre-calculating tempo map...");

        #[derive(Debug, Clone, Copy)]
        struct TempoPoint {
            absolute_tick: u64,
            absolute_ns: u64,
            tick_ns: u64,
        }

        let mut tempo_timeline: Vec<TempoPoint> = Vec::new();
        let mut last_tick = 0u64;
        let mut elapsed_ns = 0u64;
        let mut current_tempo_us = 500_000u32;
        let mut tick_ns = Self::tempo_to_tick_ns(current_tempo_us, self.header.ppqn);

        tempo_timeline.push(TempoPoint {
            absolute_tick: 0,
            absolute_ns: 0,
            tick_ns,
        });

        for event in &temp_events {
            if let TempEventData::TempoChange { new_tempo_us } = event.data {
                let delta_ticks = event.absolute_tick - last_tick;
                elapsed_ns += delta_ticks * tick_ns;

                current_tempo_us = new_tempo_us;
                tick_ns = Self::tempo_to_tick_ns(current_tempo_us, self.header.ppqn);

                tempo_timeline.push(TempoPoint {
                    absolute_tick: event.absolute_tick,
                    absolute_ns: elapsed_ns,
                    tick_ns,
                });

                last_tick = event.absolute_tick;
            }
        }

        println!("[KazuMIDIParser] Converting ticks to absolute time in parallel...");

        self.events = temp_events
            .into_par_iter()
            .map(|event| {
                let tempo_point_index =
                    tempo_timeline.partition_point(|p| p.absolute_tick <= event.absolute_tick) - 1;
                let base_tempo_point = tempo_timeline[tempo_point_index];
                let delta_ticks_from_base = event.absolute_tick - base_tempo_point.absolute_tick;
                let final_ns = base_tempo_point.absolute_ns
                    + (delta_ticks_from_base * base_tempo_point.tick_ns);

                match event.data {
                    TempEventData::Midi {
                        status,
                        data1,
                        data2,
                    } => Some(MidiEvent {
                        absolute_ns: final_ns,
                        status,
                        data1,
                        data2,
                        track_index: event.track_index,
                        sysex_data: None,
                    }),
                    TempEventData::SysEx { data } => Some(MidiEvent {
                        absolute_ns: final_ns,
                        status: 0xF0,
                        data1: 0,
                        data2: 0,
                        track_index: event.track_index,
                        sysex_data: Some(data),
                    }),
                    TempEventData::TempoChange { .. } => None,
                }
            })
            .filter_map(|e| e)
            .collect();

        self.is_parsed = true;
        Ok(())
    }

    pub fn get_events(&self) -> &Vec<MidiEvent> {
        &self.events
    }

    pub fn get_track_event_indices(&self) -> Vec<Vec<usize>> {
        let mut track_event_indices: Vec<Vec<usize>> =
            vec![Vec::new(); self.header.tracks as usize];
        for (index, event) in self.events.iter().enumerate() {
            if (event.track_index as usize) < track_event_indices.len() {
                track_event_indices[event.track_index as usize].push(index);
            }
        }
        track_event_indices
    }
}
