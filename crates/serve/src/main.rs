//! `serve [PORT] [DIR] [--bind ADDRESS] [-v]`: a local web server for the browser build (default 8770 and web/,
//! through web/serve.sh), on 127.0.0.1 unless `--bind`.
//!
//! Static files with HTTP Range requests (browsers seek in the music and movie files). A file with a compressed copy
//! beside it (NAME.br / NAME.gz, made by web/compress.sh, not older than NAME) goes out compressed when the
//! browser takes that encoding, with X-Size, its size as stored, for the page's progress bars. The page, the build's
//! name and the data lists are never cached (`/`, `.html`, `.json`, `version.txt`): a browser with an old page and a
//! new game would break. HTTP/1.0: one request per connection, a thread each. Symlinks in the folder are followed
//! (web/assets -> ../assets); a request's `..` never leaves it.

use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let all: Vec<String> = std::env::args().skip(1).collect();
    let bind = all.iter().position(|a| a == "--bind").and_then(|i| all.get(i + 1)).cloned();
    let verbose = all.iter().any(|a| a == "-v");
    let args: Vec<&String> = all.iter().filter(|a| !a.starts_with('-') && Some(*a) != bind.as_ref()).collect();
    let bind = bind.unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = match args.first() {
        Some(p) => p.parse().with_context(|| format!("bad port {p}"))?,
        None => 8770,
    };
    let dir = match args.get(1) {
        Some(d) => PathBuf::from(d),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web"),
    };
    let dir = dir.canonicalize().with_context(|| format!("cannot serve {}", dir.display()))?;
    let listener = TcpListener::bind((bind.as_str(), port)).with_context(|| format!("cannot listen on {bind}:{port}"))?;
    println!("serving {} at http://{bind}:{port}/", dir.display());
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let dir = dir.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle(conn, &dir, verbose)
                && verbose
            {
                eprintln!("{e}"); // a dropped connection is the browser's business
            }
        });
    }
    Ok(())
}

fn handle(mut conn: TcpStream, dir: &Path, verbose: bool) -> std::io::Result<()> {
    // (the request's head up to 8 KiB: a client cannot grow it without end)
    let mut reader = BufReader::new(conn.try_clone()?.take(8192));
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if !line.ends_with('\n') {
        return error(&mut conn, 400, "Bad request");
    }
    let (mut range, mut accepted) = (None, String::new());
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            if k.trim().eq_ignore_ascii_case("range") {
                range = Some(v.trim().to_string());
            } else if k.trim().eq_ignore_ascii_case("accept-encoding") {
                accepted = v.to_string();
            }
        }
    }
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let head = method == "HEAD";
    if method != "GET" && !head {
        return error(&mut conn, 501, "Unsupported method");
    }
    let raw = target.split(['?', '#']).next().unwrap_or("");
    if verbose {
        eprintln!("{method} {target}");
    }
    let no_cache = if [".html", ".json", "version.txt", "/"].iter().any(|e| raw.ends_with(e)) { "Cache-Control: no-cache\r\n" } else { "" };
    let path = translate_path(dir, raw);
    let path = if path.is_dir() {
        if !raw.ends_with('/') {
            let loc = format!("{raw}/");
            return write!(conn, "HTTP/1.0 301 Moved Permanently\r\nLocation: {loc}\r\nContent-Length: 0\r\n\r\n");
        }
        path.join("index.html")
    } else if raw.ends_with('/') {
        return error(&mut conn, 404, "File not found");
    } else {
        path
    };
    let Ok(mut f) = File::open(&path) else {
        return error(&mut conn, 404, "File not found");
    };
    let meta = f.metadata()?;
    let size = meta.len();
    let ctype = guess_type(&path);
    if let Some((mut packed, name)) = compressed(&path, &meta, &accepted) {
        let len = packed.metadata()?.len();
        write!(
            conn,
            "HTTP/1.0 200 OK\r\nContent-Type: {ctype}\r\nContent-Encoding: {name}\r\nContent-Length: {len}\r\n\
             X-Size: {size}\r\nVary: Accept-Encoding\r\n{no_cache}\r\n"
        )?;
        if !head {
            std::io::copy(&mut packed, &mut conn)?;
        }
        return Ok(());
    }
    let (start, end) = match range.as_deref().and_then(|r| parse_range(r, size)) {
        Some(Ok(r)) => r,
        Some(Err(())) => {
            return write!(conn, "HTTP/1.0 416 Range Not Satisfiable\r\nContent-Range: bytes */{size}\r\n\r\n");
        }
        None => {
            write!(conn, "HTTP/1.0 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {size}\r\nAccept-Ranges: bytes\r\n{no_cache}\r\n")?;
            if !head {
                std::io::copy(&mut f, &mut conn)?;
            }
            return Ok(());
        }
    };
    f.seek(SeekFrom::Start(start))?;
    write!(
        conn,
        "HTTP/1.0 206 Partial Content\r\nContent-Type: {ctype}\r\nContent-Range: bytes {start}-{end}/{size}\r\n\
         Content-Length: {}\r\nAccept-Ranges: bytes\r\n\r\n",
        end - start + 1
    )?;
    if !head {
        std::io::copy(&mut f.take(end - start + 1), &mut conn)?;
    }
    Ok(())
}

/// NAME.br / NAME.gz beside the file, when the browser takes that encoding and the copy is not older (by a second).
fn compressed(path: &Path, meta: &std::fs::Metadata, accepted: &str) -> Option<(File, &'static str)> {
    let modified = meta.modified().ok()?;
    [("br", "br"), ("gz", "gzip")].into_iter().find_map(|(ext, name)| {
        let packed = path.with_file_name(format!("{}.{ext}", path.file_name()?.to_str()?));
        let pm = packed.metadata().ok().filter(|m| m.is_file())?.modified().ok()?;
        let fresh = pm + std::time::Duration::from_secs(1) >= modified;
        (accepted.contains(name) && fresh).then(|| File::open(&packed).ok().map(|f| (f, name))).flatten()
    })
}

fn error(conn: &mut TcpStream, code: u16, msg: &str) -> std::io::Result<()> {
    write!(conn, "HTTP/1.0 {code} {msg}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{msg}", msg.len())
}

/// SimpleHTTPRequestHandler.translate_path: the URL path, %-decoded and normalised, under `dir`; `..` that would
/// climb above the root is dropped, as is a word that is not one plain name (Windows: `\\`, a drive, a root).
fn translate_path(dir: &Path, raw: &str) -> PathBuf {
    let mut words: Vec<String> = Vec::new();
    let name = |w: &str| {
        let mut c = Path::new(w).components();
        !w.contains(['\\', ':']) && matches!((c.next(), c.next()), (Some(std::path::Component::Normal(_)), None))
    };
    for w in percent_decode(raw).split('/') {
        match w {
            ".." => {
                words.pop();
            }
            w if name(w) => words.push(w.to_string()),
            _ => {}
        }
    }
    words.iter().fold(dir.to_path_buf(), |p, w| p.join(w))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b[i], hex) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The script's `bytes=(\d*)-(\d*)$`: None = no usable range (the whole file), Err = 416.
fn parse_range(r: &str, size: u64) -> Option<Result<(u64, u64), ()>> {
    let (a, b) = r.strip_prefix("bytes=")?.split_once('-')?;
    if !a.chars().chain(b.chars()).all(|c| c.is_ascii_digit()) || (a.is_empty() && b.is_empty()) {
        return None;
    }
    let (start, end) = if !a.is_empty() {
        let end = if b.is_empty() { size.saturating_sub(1) } else { b.parse().ok()? };
        (a.parse().ok()?, end.min(size.saturating_sub(1)))
    } else {
        (size.saturating_sub(b.parse().ok()?), size.saturating_sub(1))
    };
    Some(if start >= size || start > end { Err(()) } else { Ok((start, end)) })
}

fn guess_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "wasm" => "application/wasm",
        "css" => "text/css",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "webmanifest" => "application/manifest+json",
        "woff2" => "font/woff2",
        "glb" => "model/gltf-binary",
        "gltf" => "model/gltf+json",
        "mp3" => "audio/mpeg",
        "wav" => "audio/x-wav",
        "flac" => "audio/flac",
        "avi" => "video/x-msvideo",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn ranges_as_the_script() {
        assert_eq!(parse_range("bytes=0-9", 100), Some(Ok((0, 9))));
        assert_eq!(parse_range("bytes=90-", 100), Some(Ok((90, 99))));
        assert_eq!(parse_range("bytes=-10", 100), Some(Ok((90, 99))));
        assert_eq!(parse_range("bytes=-500", 100), Some(Ok((0, 99))));
        assert_eq!(parse_range("bytes=50-500", 100), Some(Ok((50, 99))));
        assert_eq!(parse_range("bytes=100-", 100), Some(Err(())));
        assert_eq!(parse_range("bytes=9-3", 100), Some(Err(())));
        assert_eq!(parse_range("bytes=-", 100), None);
        assert_eq!(parse_range("bytes=1-2,4-5", 100), None);
        assert_eq!(parse_range("bytes=-0", 0), Some(Err(())));
    }

    #[test]
    fn compressed_copies_when_taken() {
        let d = std::env::temp_dir().join(format!("serve_test_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("a.json");
        std::fs::write(&f, "{}").unwrap();
        std::fs::write(d.join("a.json.gz"), "x").unwrap();
        let meta = f.metadata().unwrap();
        assert_eq!(compressed(&f, &meta, "gzip, deflate").map(|c| c.1), Some("gzip"));
        assert!(compressed(&f, &meta, "deflate").is_none());
        assert!(compressed(&d.join("b.json"), &meta, "gzip").is_none());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn no_escape() {
        let d = Path::new("/srv");
        assert_eq!(translate_path(d, "/../../etc/passwd"), Path::new("/srv/etc/passwd"));
        assert_eq!(translate_path(d, "/a/%2e%2e/%2E%2E/b"), Path::new("/srv/b"));
        assert_eq!(translate_path(d, "/models/x.glb"), Path::new("/srv/models/x.glb"));
        assert_eq!(translate_path(d, "/a/..%5C..%5Cetc/b"), Path::new("/srv/a/b"));
        assert_eq!(translate_path(d, "/C:%5Cwindows/c:/x"), Path::new("/srv/x"));
        assert_eq!(translate_path(d, "/%5C%5Cserver%5Cshare/./x"), Path::new("/srv/x"));
    }
}
