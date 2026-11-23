use std::io::{self, Read, Write};

const MAX_FRAME_BYTES: usize = 16 * 1024;
const SUCCESS_CAPABILITY: &str = "stdio_plugin_smoke";
const UNCERTAIN_CAPABILITY: &str = "stdio_plugin_uncertain";
const HANG_CAPABILITY: &str = "stdio_plugin_hang";

fn read_frame(input: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut header = [0; 4];
    input.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    if !(1..=MAX_FRAME_BYTES).contains(&length) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid frame length",
        ));
    }
    let mut payload = vec![0; length];
    input.read_exact(&mut payload)?;
    Ok(payload)
}

fn write_frame(output: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    output.write_all(&length.to_be_bytes())?;
    output.write_all(payload)?;
    output.flush()
}

fn main() -> io::Result<()> {
    let loopback = std::net::SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, 0);
    if std::net::TcpListener::bind(loopback).is_ok()
        || std::path::Path::new("/home").exists()
        || std::path::Path::new("/proc").exists()
    {
        return Err(io::Error::other("plugin cannot run without isolation"));
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();

    if read_frame(&mut input)?.as_slice() != br#"{"type":"hello","version":1}"# {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid Hello"));
    }
    write_frame(
        &mut output,
        br#"{"type":"ready","version":1,"capabilities":["stdio_plugin_smoke","stdio_plugin_uncertain","stdio_plugin_hang"]}"#,
    )?;
    let execute = read_frame(&mut input)?;
    let execute = std::str::from_utf8(&execute)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Execute is not UTF-8"))?;

    if !execute.contains(r#""type":"execute""#) || !execute.contains(r#""id":1"#) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid Execute",
        ));
    }
    if execute.contains(&format!(r#""capability":"{UNCERTAIN_CAPABILITY}""#)) {
        write_frame(&mut output, b"malformed plugin result")?;
    } else if [SUCCESS_CAPABILITY, HANG_CAPABILITY]
        .iter()
        .any(|capability| execute.contains(&format!(r#""capability":"{capability}""#)))
    {
        write_frame(
            &mut output,
            br#"{"type":"result","id":1,"outputs":{"title":"stdio plugin result"}}"#,
        )?;
        if execute.contains(&format!(r#""capability":"{HANG_CAPABILITY}""#)) {
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected capability",
        ));
    }
    Ok(())
}
