//! A `WebDAV` server over a directory, for tests: the methods the backend
//! sends, answered as RFC 4918 has a server answer them, one request per
//! connection. A request body without a length is refused, as some
//! servers refuse a chunked PUT, and so is an upload above a limit, as a
//! proxy in front of a server refuses it. As a Nextcloud, it also takes
//! uploads in parts, in the way of Nextcloud's chunked upload v2.

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use tempfile::TempDir;

/// Where a Nextcloud keeps a user's files and uploads in parts.
const FILES: &str = "/remote.php/dav/files/backup";
const UPLOADS: &str = "/remote.php/dav/uploads/backup/";

/// A running server. It stops with the test process.
pub struct Server {
    /// The URL of the served directory, ending in `/`.
    pub url: String,
    log: Arc<Mutex<Vec<String>>>,
    _uploads: Option<TempDir>,
}

impl Server {
    /// Serve `dir` under `/dav/`; with `login`, every request must carry
    /// it.
    pub fn start(dir: &Path, login: Option<(&str, &str)>) -> Server {
        Server::run(dir, login, None, "/dav", false)
    }

    /// Serve `dir` under `/dav/`, refusing an upload above `limit` bytes.
    pub fn limited(dir: &Path, limit: usize) -> Server {
        Server::run(dir, None, Some(limit), "/dav", false)
    }

    /// Serve `dir` as the files of the Nextcloud user `backup`, refusing
    /// an upload above `limit` bytes.
    pub fn nextcloud(dir: &Path, limit: usize) -> Server {
        Server::run(dir, None, Some(limit), FILES, true)
    }

    /// Serve `dir` under the address of a Nextcloud user's files, without
    /// uploads in parts.
    pub fn files_without_parts(dir: &Path) -> Server {
        Server::run(dir, None, None, FILES, false)
    }

    fn run(
        dir: &Path,
        login: Option<(&str, &str)>,
        limit: Option<usize>,
        prefix: &'static str,
        parts: bool,
    ) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let uploads = parts.then(|| TempDir::new().unwrap());
        let state = State {
            prefix,
            uploads: uploads.as_ref().map(|dir| dir.path().to_path_buf()),
            dir: dir.to_path_buf(),
            base: base.clone(),
            login: login.map(|(user, password)| {
                format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
            }),
            log: Arc::clone(&log),
            limit,
        };
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                state.serve(stream);
            }
        });
        Server {
            url: format!("{base}{prefix}/"),
            log,
            _uploads: uploads,
        }
    }

    /// Every request so far, as `METHOD path`.
    pub fn requests(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

struct State {
    prefix: &'static str,
    uploads: Option<PathBuf>,
    dir: PathBuf,
    base: String,
    login: Option<String>,
    log: Arc<Mutex<Vec<String>>>,
    limit: Option<usize>,
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        let mut found = self.headers.iter().filter(|(key, _)| key == name);
        found.next().map(|(_, value)| value.as_str())
    }
}

struct Answer {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

fn status(status: u16) -> Answer {
    Answer {
        status,
        headers: Vec::new(),
        body: Vec::new(),
    }
}

impl State {
    fn serve(&self, mut stream: TcpStream) {
        let Some(request) = read(&stream) else {
            return;
        };
        self.log
            .lock()
            .unwrap()
            .push(format!("{} {}", request.method, request.path));
        let answer = self.answer(&request);
        let mut head = format!(
            "HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\n",
            answer.status,
            answer.body.len()
        );
        for (name, value) in &answer.headers {
            write!(head, "{name}: {value}\r\n").unwrap();
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).ok();
        if request.method != "HEAD" {
            stream.write_all(&answer.body).ok();
        }
    }

    /// The file a request path names, if it lies under the prefix.
    fn file(&self, path: &str) -> Option<PathBuf> {
        let inside = path.strip_prefix(self.prefix)?.trim_matches('/');
        Some(if inside.is_empty() {
            self.dir.clone()
        } else {
            self.dir.join(inside)
        })
    }

    fn answer(&self, request: &Request) -> Answer {
        if request.header("transfer-encoding").is_some() {
            return status(411);
        }
        let above = self.limit.is_some_and(|limit| request.body.len() > limit);
        if request.method == "PUT" && above {
            return status(413);
        }
        if let Some(login) = &self.login {
            if request.header("authorization") != Some(login) {
                let mut answer = status(401);
                answer
                    .headers
                    .push(("www-authenticate", "Basic realm=\"dav\"".to_string()));
                return answer;
            }
        }
        if let Some(answer) = self.upload(request) {
            return answer;
        }
        let Some(file) = self.file(&request.path) else {
            return status(404);
        };
        let above = file.parent().is_some_and(Path::is_dir);
        match request.method.as_str() {
            "PROPFIND" => propfind(request, &file),
            "GET" | "HEAD" if file.is_file() => Answer {
                body: fs::read(&file).unwrap(),
                ..status(200)
            },
            "PUT" if !above => status(409),
            "PUT" if file.is_dir() => status(405),
            "PUT" => {
                let existed = file.exists();
                fs::write(&file, &request.body).unwrap();
                status(if existed { 204 } else { 201 })
            }
            "MKCOL" if file.exists() => status(405),
            "MKCOL" if !above => status(409),
            "MKCOL" => {
                fs::create_dir(&file).unwrap();
                status(201)
            }
            "MOVE" => self.move_to(request, &file),
            "DELETE" if file.is_file() => {
                fs::remove_file(&file).unwrap();
                status(204)
            }
            "GET" | "HEAD" | "DELETE" => status(404),
            _ => status(405),
        }
    }

    /// A request in the collection for uploads in parts, if it is one:
    /// every one names where the file goes, and the MOVE of `.file`
    /// puts the parts together there in the order of their numbers.
    fn upload(&self, request: &Request) -> Option<Answer> {
        let uploads = self.uploads.as_ref()?;
        let inside = request.path.strip_prefix(UPLOADS)?.trim_end_matches('/');
        let Some(destination) = request.header("destination") else {
            return Some(status(400));
        };
        let (id, part) = inside.split_once('/').unwrap_or((inside, ""));
        let dir = uploads.join(id);
        Some(match (request.method.as_str(), part) {
            ("MKCOL", "") if dir.exists() => status(405),
            ("MKCOL", "") => {
                fs::create_dir(&dir).unwrap();
                status(201)
            }
            ("PUT", number) if dir.is_dir() && number.parse::<u16>().is_ok() => {
                fs::write(dir.join(number), &request.body).unwrap();
                status(201)
            }
            ("MOVE", ".file") if dir.is_dir() => {
                let Some(file) = destination
                    .strip_prefix(&self.base)
                    .and_then(|path| self.file(path))
                else {
                    return Some(status(502));
                };
                let mut parts: Vec<_> = fs::read_dir(&dir)
                    .unwrap()
                    .map(|item| item.unwrap().file_name().into_string().unwrap())
                    .collect();
                parts.sort_by_key(|name| name.parse::<u16>().unwrap());
                let whole: Vec<u8> = parts
                    .iter()
                    .flat_map(|name| fs::read(dir.join(name)).unwrap())
                    .collect();
                let existed = file.exists();
                fs::write(&file, whole).unwrap();
                fs::remove_dir_all(&dir).unwrap();
                status(if existed { 204 } else { 201 })
            }
            _ => status(404),
        })
    }

    fn move_to(&self, request: &Request, file: &Path) -> Answer {
        let destination = request
            .header("destination")
            .and_then(|url| url.strip_prefix(&self.base))
            .and_then(|path| self.file(path));
        let Some(destination) = destination else {
            return status(502);
        };
        if !file.exists() {
            return status(404);
        }
        let existed = destination.exists();
        if existed && request.header("overwrite") == Some("F") {
            return status(412);
        }
        if !destination.parent().is_some_and(Path::is_dir) {
            return status(409);
        }
        fs::rename(file, &destination).unwrap();
        status(if existed { 204 } else { 201 })
    }
}

fn propfind(request: &Request, file: &Path) -> Answer {
    if !file.exists() {
        return status(404);
    }
    let mut entries = vec![(request.path.clone(), file.is_dir())];
    if request.header("depth") == Some("1") && file.is_dir() {
        for item in fs::read_dir(file).unwrap() {
            let item = item.unwrap();
            let name = item.file_name().into_string().unwrap();
            let href = format!("{}/{name}", request.path.trim_end_matches('/'));
            entries.push((href, item.path().is_dir()));
        }
    }
    // `D:` as Apache writes it; the backend must not rely on `d:`.
    let mut xml = String::from(r#"<?xml version="1.0"?><D:multistatus xmlns:D="DAV:">"#);
    for (href, collection) in entries {
        let kind = if collection { "<D:collection/>" } else { "" };
        write!(
            xml,
            "<D:response><D:href>{href}</D:href><D:propstat><D:prop><D:resourcetype>{kind}</D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
        )
        .unwrap();
    }
    xml.push_str("</D:multistatus>");
    Answer {
        headers: vec![("content-type", "application/xml".to_string())],
        body: xml.into_bytes(),
        ..status(207)
    }
}

fn read(stream: &TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .map_or(0, |(_, value)| value.parse().unwrap());
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}
