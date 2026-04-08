use std::io;
use std::net::UdpSocket;

pub const VRCHAT_OSC_TARGET: &str = "127.0.0.1:9000";
pub const VRCHAT_CHATBOX_CHAR_LIMIT: usize = 144;
pub const VRCHAT_CHATBOX_MAX_LINES: u8 = 9;

pub fn clamp_history_count(count: u8) -> u8 {
    count.clamp(1, VRCHAT_CHATBOX_MAX_LINES)
}

pub fn truncate_for_chatbox(text: &str) -> String {
    let char_count = text.chars().count();
    if char_count <= VRCHAT_CHATBOX_CHAR_LIMIT {
        return text.to_string();
    }

    text.chars()
        .skip(char_count - VRCHAT_CHATBOX_CHAR_LIMIT)
        .collect()
}

pub fn send_chatbox_input(text: &str) -> io::Result<()> {
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    let packet = build_chatbox_input_packet(text);
    let _ = socket.send_to(&packet, VRCHAT_OSC_TARGET)?;
    Ok(())
}

fn build_chatbox_input_packet(text: &str) -> Vec<u8> {
    let mut packet = Vec::new();
    append_osc_string(&mut packet, "/chatbox/input");
    append_osc_string(&mut packet, ",sT");
    append_osc_string(&mut packet, text);
    packet
}

fn append_osc_string(packet: &mut Vec<u8>, value: &str) {
    packet.extend_from_slice(value.as_bytes());
    packet.push(0);

    while packet.len() % 4 != 0 {
        packet.push(0);
    }
}
