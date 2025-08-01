use std::error::Error as StdError;
use std::fs::File;
use std::io::Read;

use rayon::slice::ParallelSliceMut;

#[derive(Debug)]
pub struct MidiHeader {
    pub format: u16,
    pub tracks: u16,
    pub ppqn: u16,
}

#[derive(Debug)]
pub struct MidiEvent {
    pub delta_ns: u64,
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
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
}

#[derive(Debug)]
struct TempEvent {
    absolute_tick: u64,
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

        // --- Phase 1: Collect events from all tracks on a tick basis ---
        let mut temp_events: Vec<TempEvent> = Vec::new();

        for track_index in 0..self.header.tracks {
            file.read_exact(&mut buffer32)?;
            if buffer32 != *b"MTrk" {
                return Err(format!(
                    "Expected MTrk, found {:?} at track {}",
                    buffer32, track_index
                )
                .into());
            }

            file.read_exact(&mut buffer32)?;
            let track_length = u32::from_be_bytes(buffer32);
            let mut track_data = vec![0; track_length as usize];
            file.read_exact(&mut track_data)?;

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
                    return Err("Running status without previous status".into());
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
                            temp_events.push(TempEvent {
                                absolute_tick,
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

                    temp_events.push(TempEvent {
                        absolute_tick,
                        data: TempEventData::Midi {
                            status,
                            data1,
                            data2,
                        },
                    });
                } else {
                    // TODO: System message
                }
            }
            println!(
                "[KazuMIDIParser] Track {}/{} parsed ({} bytes), collected {} temp events",
                track_index + 1,
                self.header.tracks,
                track_length,
                temp_events.len()
            );
        }

        println!("[KazuMIDIParser] Sorting merged events...");
        // --- Phase 2: Sort events and convert ticks to time ---
        temp_events.par_sort_by_key(|e| e.absolute_tick);

        self.events.clear();
        let mut elapsed_ns = 0u64;
        let mut last_tick = 0u64;
        let mut current_tempo_us = 500_000u32;
        let mut tick_ns = Self::tempo_to_tick_ns(current_tempo_us, self.header.ppqn);

        println!("[KazuMIDIParser] Converting ticks to absolute time...");
        for event in temp_events {
            let delta_ticks = event.absolute_tick - last_tick;
            elapsed_ns += delta_ticks * tick_ns;

            match event.data {
                TempEventData::Midi {
                    status,
                    data1,
                    data2,
                } => {
                    self.events.push(MidiEvent {
                        delta_ns: elapsed_ns,
                        status,
                        data1,
                        data2,
                    });
                }
                TempEventData::TempoChange { new_tempo_us } => {
                    current_tempo_us = new_tempo_us;
                    tick_ns = Self::tempo_to_tick_ns(current_tempo_us, self.header.ppqn);
                }
            }
            last_tick = event.absolute_tick;
        }

        self.is_parsed = true;
        Ok(())
    }

    pub fn get_events(&self) -> &Vec<MidiEvent> {
        &self.events
    }
}
