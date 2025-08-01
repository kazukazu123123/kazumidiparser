use std::ffi::{c_char, CStr};

use kazumidiparser_core::MidiParser;

pub enum KazuMIDIParserPtr {}

#[repr(C)]
pub struct KazuMIDIParserHeader {
    format: u16,
    tracks: u16,
    ppqn: u16,
}

#[repr(C)]
pub struct KazuMIDIParserMidiEvent {
    absolute_ns: u64,
    status: u8,
    data1: u8,
    data2: u8,
}

#[unsafe(no_mangle)]
pub extern "C" fn midiparser_new() -> *mut KazuMIDIParserPtr {
    let midi_parser = Box::new(MidiParser::new());
    Box::into_raw(midi_parser) as *mut KazuMIDIParserPtr
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn midiparser_parse_midi_file(
    midiparser_ptr: *mut KazuMIDIParserPtr,
    midi_path: *const c_char,
) -> bool {
    if midiparser_ptr.is_null() || midi_path.is_null() {
        return false;
    }

    let midiparser = unsafe { &mut *(midiparser_ptr as *mut MidiParser) };

    let c_str = unsafe { CStr::from_ptr(midi_path) };
    let rust_path = match c_str.to_str() {
        Ok(s) => s,
        Err(_) => return false,
    };

    match midiparser.parse_file(rust_path) {
        Ok(_) => true,
        Err(_) => false,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn midiparser_get_header(
    midiparser_ptr: *mut KazuMIDIParserPtr,
) -> *mut KazuMIDIParserHeader {
    if midiparser_ptr.is_null() {
        return std::ptr::null_mut();
    }
    let midiparser: &MidiParser = unsafe { &*(midiparser_ptr as *mut MidiParser) };

    match midiparser.get_header() {
        Some(header) => {
            let c_header = KazuMIDIParserHeader {
                format: header.format,
                tracks: header.tracks,
                ppqn: header.ppqn,
            };
            Box::into_raw(Box::new(c_header))
        }
        None => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn midiparser_get_events(
    midiparser_ptr: *mut KazuMIDIParserPtr,
) -> *mut KazuMIDIParserMidiEvent {
    if midiparser_ptr.is_null() {
        return std::ptr::null_mut();
    }
    let midiparser: &MidiParser = unsafe { &*(midiparser_ptr as *mut MidiParser) };

    let rust_events = midiparser.get_events();

    let mut c_events: Vec<KazuMIDIParserMidiEvent> = rust_events
        .iter()
        .map(|event| KazuMIDIParserMidiEvent {
            absolute_ns: event.absolute_ns,
            status: event.status,
            data1: event.data1,
            data2: event.data2,
        })
        .collect();

    c_events.shrink_to_fit();

    let ptr = c_events.as_mut_ptr();
    std::mem::forget(c_events);

    ptr
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn midiparser_get_events_len(
    midiparser_ptr: *mut KazuMIDIParserPtr,
) -> usize {
    if midiparser_ptr.is_null() {
        return 0;
    }
    let midiparser: &MidiParser = unsafe { &*(midiparser_ptr as *mut MidiParser) };
    midiparser.get_events().len()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn midiparser_events_free(
    events_ptr: *mut KazuMIDIParserMidiEvent,
    len: usize,
) {
    if !events_ptr.is_null() {
        unsafe {
            let _ = Vec::from_raw_parts(events_ptr, len, len);
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn midiparser_header_free(header_ptr: *mut KazuMIDIParserHeader) {
    if !header_ptr.is_null() {
        drop(unsafe { Box::from_raw(header_ptr) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn midiparser_free(midiparser_ptr: *mut KazuMIDIParserPtr) {
    if !midiparser_ptr.is_null() {
        drop(unsafe { Box::from_raw(midiparser_ptr as *mut MidiParser) });
    }
}
