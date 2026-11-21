use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Status-handskakningen accepterar vilken protokollversion som helst; 47 är
/// samma värde som panelens ping använder och fungerar mot vanilla, Paper och
/// de flesta proxyservrar.
const PROTOCOL_VERSION: i32 = 47;
const STATUS_TIMEOUT: Duration = Duration::from_millis(800);
const MAX_PACKET: i32 = 2 * 1024 * 1024;

/// Spelarantalet ur serverns server-list-svar. `None` betyder att servern
/// inte svarade (startar, har annan port eller är inte en Minecraft-server);
/// då behåller telemetrin det som konsolraderna har räknat.
pub async fn players_online(host: &str, port: u16) -> Option<i64> {
    timeout(STATUS_TIMEOUT, query(host, port))
        .await
        .ok()
        .flatten()
}

async fn query(host: &str, port: u16) -> Option<i64> {
    let mut stream = TcpStream::connect((host, port)).await.ok()?;
    stream.write_all(&handshake(host, port)).await.ok()?;
    // Status request: längd 1, packet-id 0.
    stream.write_all(&[0x01, 0x00]).await.ok()?;

    let length = read_varint(&mut stream).await?;
    if length <= 0 || length > MAX_PACKET {
        return None;
    }

    let mut payload = vec![0u8; usize::try_from(length).ok()?];
    stream.read_exact(&mut payload).await.ok()?;
    parse_status(&payload)
}

fn handshake(host: &str, port: u16) -> Vec<u8> {
    let mut body = Vec::new();
    write_varint(&mut body, 0x00);
    write_varint(&mut body, PROTOCOL_VERSION);
    write_string(&mut body, host);
    body.extend_from_slice(&port.to_be_bytes());
    write_varint(&mut body, 1);

    let mut packet = Vec::with_capacity(body.len() + 5);
    write_varint(&mut packet, i32::try_from(body.len()).unwrap_or(0));
    packet.extend_from_slice(&body);
    packet
}

fn parse_status(payload: &[u8]) -> Option<i64> {
    let mut offset = 0;
    if read_varint_at(payload, &mut offset)? != 0 {
        return None;
    }

    let length = usize::try_from(read_varint_at(payload, &mut offset)?).ok()?;
    let end = offset.checked_add(length)?;
    let json = payload.get(offset..end)?;
    let value: Value = serde_json::from_slice(json).ok()?;
    value.get("players")?.get("online")?.as_i64()
}

async fn read_varint<R: AsyncRead + Unpin>(reader: &mut R) -> Option<i32> {
    let mut result = 0i32;
    for shift in (0..35).step_by(7) {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).await.ok()?;
        result |= i32::from(byte[0] & 0x7f) << shift;
        if byte[0] & 0x80 == 0 {
            return Some(result);
        }
    }
    None
}

fn read_varint_at(buffer: &[u8], offset: &mut usize) -> Option<i32> {
    let mut result = 0i32;
    for shift in (0..35).step_by(7) {
        let byte = *buffer.get(*offset)?;
        *offset += 1;
        result |= i32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(result);
        }
    }
    None
}

fn write_varint(out: &mut Vec<u8>, value: i32) {
    let mut remaining = value as u32;
    loop {
        let mut current = (remaining & 0x7f) as u8;
        remaining >>= 7;
        if remaining != 0 {
            current |= 0x80;
        }
        out.push(current);
        if remaining == 0 {
            return;
        }
    }
}

fn write_string(out: &mut Vec<u8>, value: &str) {
    write_varint(out, i32::try_from(value.len()).unwrap_or(0));
    out.extend_from_slice(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_packet(json: &str) -> Vec<u8> {
        let mut payload = Vec::new();
        write_varint(&mut payload, 0);
        write_varint(&mut payload, i32::try_from(json.len()).unwrap());
        payload.extend_from_slice(json.as_bytes());
        payload
    }

    #[test]
    fn handshake_has_the_status_shape() {
        let packet = handshake("craftpilot-096d46aa", 25565);
        assert_eq!(usize::from(packet[0]), packet.len() - 1);
        assert!(packet
            .windows("craftpilot-096d46aa".len())
            .any(|window| window == b"craftpilot-096d46aa"));
        assert!(packet.contains(&0x63) && packet.contains(&0xdd));
    }

    #[test]
    fn parses_online_players() {
        let packet = status_packet(r#"{"players":{"online":3,"max":20}}"#);
        assert_eq!(parse_status(&packet), Some(3));
    }

    #[test]
    fn rejects_a_response_without_players() {
        assert_eq!(parse_status(&status_packet("{}")), None);
        assert_eq!(parse_status(&status_packet("not json")), None);
        assert_eq!(parse_status(&[]), None);
    }

    #[test]
    fn varints_round_trip() {
        for value in [0, 1, 127, 128, 25565, 1_000_000] {
            let mut bytes = Vec::new();
            write_varint(&mut bytes, value);
            let mut offset = 0;
            assert_eq!(read_varint_at(&bytes, &mut offset), Some(value));
            assert_eq!(offset, bytes.len());
        }
    }
}
