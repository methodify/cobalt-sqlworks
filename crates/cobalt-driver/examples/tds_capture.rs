//! Dev helper: a fake TDS server that answers PRELOGIN, terminates TLS, and dumps the LOGIN7
//! record a client sends. Used to diff what other drivers put on the wire.
//! Usage: PFX=cert.pfx PFX_PASS=x cargo run -p cobalt-driver --example tds_capture -- 127.0.0.1:14330 out.bin
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

#[derive(Debug)]
struct Tds {
    s: TcpStream,
    handshaking: bool,
    rbuf: Vec<u8>,
    rpos: usize,
}

impl Tds {
    fn read_exact_raw(&mut self, n: usize) -> std::io::Result<Vec<u8>> {
        let mut v = vec![0u8; n];
        self.s.read_exact(&mut v)?;
        Ok(v)
    }
    fn read_packet_raw(&mut self) -> std::io::Result<(u8, u8, Vec<u8>)> {
        let h = self.read_exact_raw(8)?;
        let len = u16::from_be_bytes([h[2], h[3]]) as usize;
        let p = self.read_exact_raw(len - 8)?;
        Ok((h[0], h[1], p))
    }
    fn write_packet_raw(&mut self, ty: u8, payload: &[u8]) -> std::io::Result<()> {
        let mut v = vec![ty, 0x01, 0, 0, 0, 0, 1, 0];
        let len = (payload.len() + 8) as u16;
        v[2..4].copy_from_slice(&len.to_be_bytes());
        v.extend_from_slice(payload);
        self.s.write_all(&v)
    }
}

impl Read for Tds {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.handshaking {
            return self.s.read(buf);
        }
        if self.rpos >= self.rbuf.len() {
            let (_, _, p) = self.read_packet_raw()?;
            self.rbuf = p;
            self.rpos = 0;
        }
        let n = buf.len().min(self.rbuf.len() - self.rpos);
        buf[..n].copy_from_slice(&self.rbuf[self.rpos..self.rpos + n]);
        self.rpos += n;
        Ok(n)
    }
}
impl Write for Tds {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.handshaking {
            self.write_packet_raw(0x12, buf)?;
            Ok(buf.len())
        } else {
            self.s.write(buf)
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.s.flush()
    }
}

fn prelogin_response() -> Vec<u8> {
    let opts: Vec<(u8, Vec<u8>)> = vec![
        (0x00, vec![0x10, 0x00, 0x03, 0xE8, 0x00, 0x00]),
        (0x01, vec![0x01]), // ENCRYPT_ON
        (0x02, vec![0x00]),
        (0x03, vec![]),
        (0x04, vec![0x00]),
        (0x06, vec![0x01]), // FEDAUTHREQUIRED echo
    ];
    let table_len = opts.len() * 5 + 1;
    let mut table = Vec::new();
    let mut data = Vec::new();
    for (t, d) in &opts {
        let off = (table_len + data.len()) as u16;
        table.push(*t);
        table.extend_from_slice(&off.to_be_bytes());
        table.extend_from_slice(&(d.len() as u16).to_be_bytes());
        data.extend_from_slice(d);
    }
    table.push(0xFF);
    table.extend(data);
    table
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn ustr(b: &[u8], off: usize, cch: usize) -> String {
    let u: Vec<u16> = (0..cch).map(|i| u16le(b, off + 2 * i)).collect();
    String::from_utf16_lossy(&u)
}

fn decode_login7(b: &[u8]) {
    println!(
        "Length={} TDSVersion={:#010x} PacketSize={} ClientProgVer={:#010x} ClientPID={} ConnectionID={}",
        u32le(b, 0),
        u32le(b, 4),
        u32le(b, 8),
        u32le(b, 12),
        u32le(b, 16),
        u32le(b, 20)
    );
    println!(
        "OptionFlags1={:#04x} OptionFlags2={:#04x} TypeFlags={:#04x} OptionFlags3={:#04x} ClientTimeZone={} ClientLCID={:#x}",
        b[24],
        b[25],
        b[26],
        b[27],
        u32le(b, 28) as i32,
        u32le(b, 32)
    );
    let names = ["HostName", "UserName", "Password", "AppName", "ServerName", "Extension", "CltIntName", "Language", "Database"];
    let mut o = 36;
    let mut ext_off = None;
    for n in names {
        let ib = u16le(b, o) as usize;
        let cb = u16le(b, o + 2) as usize;
        if n == "Extension" {
            println!("  ibExtension={ib} cbExtension={cb}");
            if cb == 4 {
                ext_off = Some(u32le(b, ib) as usize);
            }
        } else if n == "Password" {
            println!("  {n}: cch={cb}");
        } else {
            println!("  {n}: {:?}", ustr(b, ib, cb));
        }
        o += 4;
    }
    println!("  ClientID={:02x?}", &b[o..o + 6]);
    o += 6;
    for n in ["SSPI", "AtchDBFile", "ChangePassword"] {
        println!("  {n}: ib={} cb={}", u16le(b, o), u16le(b, o + 2));
        o += 4;
    }
    println!("  cbSSPILong={}", u32le(b, o));
    if let Some(mut p) = ext_off {
        println!("FeatureExt @ {p}:");
        while p < b.len() {
            let id = b[p];
            if id == 0xFF {
                println!("  TERMINATOR");
                break;
            }
            let len = u32le(b, p + 1) as usize;
            let data = &b[p + 5..p + 5 + len];
            if id == 0x02 {
                let opts = data[0];
                let tlen = u32le(data, 1) as usize;
                let tok = ustr(data, 5, tlen / 2);
                println!(
                    "  FEDAUTH len={len} options={opts:#04x} (lib={} echo={}) token_len_bytes={tlen} token_prefix={:?} trailing={} bytes",
                    opts >> 1,
                    opts & 1,
                    &tok[..tok.len().min(24)],
                    len - 5 - tlen
                );
            } else {
                println!("  feature {id:#04x} len={len} data={:02x?}", &data[..data.len().min(40)]);
            }
            p += 5 + len;
        }
    }
}

fn main() {
    let addr = std::env::args().nth(1).unwrap_or("127.0.0.1:14330".into());
    let out = std::env::args().nth(2).unwrap_or("login7.bin".into());
    let pfx = std::fs::read(std::env::var("PFX").expect("PFX")).unwrap();
    let id = native_tls::Identity::from_pkcs12(&pfx, &std::env::var("PFX_PASS").unwrap_or_default()).unwrap();
    let acceptor = native_tls::TlsAcceptor::new(id).unwrap();
    let l = TcpListener::bind(&addr).unwrap();
    eprintln!("listening on {addr}");
    let (s, peer) = l.accept().unwrap();
    eprintln!("connection from {peer}");
    let mut w = Tds { s, handshaking: true, rbuf: vec![], rpos: 0 };
    let (ty, st, p) = w.read_packet_raw().unwrap();
    println!("PRELOGIN packet type={ty:#04x} status={st:#04x} len={}", p.len());
    let mut i = 0;
    while i < p.len() && p[i] != 0xFF {
        let t = p[i];
        let off = u16::from_be_bytes([p[i + 1], p[i + 2]]) as usize;
        let len = u16::from_be_bytes([p[i + 3], p[i + 4]]) as usize;
        println!("  prelogin option {t:#04x} len={len} data={:02x?}", &p[off..off + len]);
        i += 5;
    }
    w.write_packet_raw(0x04, &prelogin_response()).unwrap();
    let mut tls = acceptor.accept(w).expect("tls accept");
    tls.get_mut().handshaking = false;
    println!("TLS established");
    let mut login = Vec::new();
    loop {
        let mut h = [0u8; 8];
        tls.read_exact(&mut h).unwrap();
        let len = u16::from_be_bytes([h[2], h[3]]) as usize;
        let mut pl = vec![0u8; len - 8];
        tls.read_exact(&mut pl).unwrap();
        println!("packet type={:#04x} status={:#04x} len={len}", h[0], h[1]);
        login.extend(pl);
        if h[1] & 1 == 1 {
            break;
        }
    }
    std::fs::write(&out, &login).unwrap();
    println!("LOGIN7 total {} bytes -> {out}", login.len());
    decode_login7(&login);
}
