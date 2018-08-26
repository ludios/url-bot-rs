/*
 * url-bot-rs
 *
 * URL parsing IRC bot
 *
 */

extern crate irc;
extern crate hyper;
extern crate curl;
extern crate htmlescape;
extern crate regex;
extern crate rusqlite;
extern crate time;
extern crate docopt;
#[macro_use]
extern crate serde_derive;

use docopt::Docopt;
use regex::Regex;
use curl::easy::{Easy2, Handler, WriteError, List};
use irc::client::prelude::*;
use htmlescape::decode_html;
use rusqlite::Connection;
use std::process;

/* docopt usage string */
const USAGE: &'static str = "
URL munching IRC bot.

Usage:
    url-bot-rs [options] [--db=PATH]

Options:
    -h --help       Show this help message.
    -d --db=PATH    Use a sqlite database at PATH.
    -c --conf=PATH  Use configuration file at PATH [default: ./config.toml].
";

#[derive(Debug, Deserialize, Default)]
struct Args {
    flag_db: String,
    flag_conf: String,
}

#[derive(Debug)]
struct LogEntry<'a> {
    id: i32,
    title: String,
    url: String,
    prefix: &'a String,
    channel: String,
    time_created: String,
}

fn add_log(db: &Connection, e: &LogEntry) {
    let u: Vec<_> = e.prefix
        .split("!")
        .collect();
    match db.execute("INSERT INTO posts (title, url, user, channel, time_created)
        VALUES (?1, ?2, ?3, ?4, ?5)",
        &[&e.title,
          &e.url,
          &String::from(u[0]),
          &e.channel,
          &time::now().to_local().ctime().to_string()])
    {
        Err(e) => {eprintln!("SQL error: {}", e); process::exit(1)},
        _      => (),
    }
}

#[derive(Debug)]
struct PrevPost {
    user: String,
    time_created: String,
    channel: String
}

fn check_prepost(db: &Connection, e: &LogEntry) -> Option<PrevPost>
{
    let query = format!("SELECT user, time_created, channel
                         FROM posts
                         WHERE title LIKE \"{}\"",
            e.title.clone()
            .replace("\"", "\"\"")
            .replace("'", "''"));

    let mut st = db.prepare(&query).unwrap();

    let mut res = st.query_map(&[], |r| {
        PrevPost {
            user: r.get(0),
            time_created: r.get(1),
            channel: r.get(2)
        }
        }).unwrap();

    match res.nth(0) {
        Some(r) => Some(r.unwrap()),
        None    => None
    }
}

/* Message { tags: None, prefix: Some("edcragg!edcragg@ip"), command: PRIVMSG("#music", "test") } */

fn main() {

    let args: Args = Docopt::new(USAGE)
                     .and_then(|d| d.deserialize())
                     .unwrap_or_else(|e| e.exit());

    let db;
    if !args.flag_db.is_empty()
    {
        /* open the sqlite database for logging */
        /* TODO: get database path from configuration */
        /* TODO: make logging optional */
        println!("Using database at: {}", args.flag_db);

        db = Connection::open(&args.flag_db).unwrap();
        db.execute("CREATE TABLE IF NOT EXISTS posts (
            id              INTEGER PRIMARY KEY,
            title           TEXT NOT NULL,
            url             TEXT NOT NULL,
            user            TEXT NOT NULL,
            channel         TEXT NOT NULL,
            time_created    TEXT NOT NULL
            )", &[]).unwrap();
    } else {
        db = Connection::open_in_memory().unwrap();
    }

    /* configure IRC */
    let server = IrcServer::new(args.flag_conf.clone());
    let server = match server {
        Ok(serv) => serv,
        Err(err) => {
            eprintln!("Error: {}", err);
            process::exit(1);
        },
    };
    println!("Using configuration at: {}", args.flag_conf);

    server.identify().unwrap();

    server.for_each_incoming(|message| {

        match message.command {

            Command::PRIVMSG(ref target, ref msg) => {

                let tokens: Vec<_> = msg.split_whitespace().collect();

                for t in tokens {
                    let mut title = None;

                    let url;
                    match t.parse::<hyper::Uri>() {
                        Ok(u) => { url = u; }
                        _     => { continue; }
                    }

                    match url.scheme() {
                        Some("http")  => { title = resolve_url(t); }
                        Some("https") => { title = resolve_url(t); }
                        _ => ()
                    }

                    match title {
                        Some(s) => {
                            let entry = LogEntry {
                                id: 0,
                                title: s.clone(),
                                url: t.clone().to_string(),
                                prefix: &message.prefix.clone().unwrap(),
                                channel: target.to_string(),
                                time_created: "".to_string()
                            };

                            /* check for pre-post */
                            let p = check_prepost(&db, &entry);
                            let msg = match p {
                                Some(p) => {
                                    format!("⤷ {} → {} {} ({})",
                                            s,
                                            p.time_created,
                                            p.user,
                                            p.channel)
                                },
                                None    => {
                                    /* add log entry to database */
                                    if !args.flag_db.is_empty() {
                                        add_log(&db, &entry);
                                    }
                                    format!("⤷ {}", s)
                                }
                            };

                            /* send the IRC response */
                            server.send_privmsg(
                                message.response_target().unwrap_or(target), &msg
                            ).unwrap();

                        }
                        _ => ()
                    }
                }
            }
            _ => (),
        }
    }).unwrap()
}

#[derive(Debug)]
struct Collector(Vec<u8>);

impl Handler for Collector {
    fn write(&mut self, data: &[u8]) -> Result<usize, WriteError> {
        self.0.extend_from_slice(data);
        Ok(data.len())
    }
}

fn resolve_url(url: &str) -> Option<String> {

    println!("RESOLVE {}", url);

    let mut easy = Easy2::new(Collector(Vec::new()));

    easy.get(true).unwrap();
    easy.url(url).unwrap();
    easy.follow_location(true).unwrap();
    easy.useragent("url-bot-rs/0.1").unwrap();

    let mut headers = List::new();
    headers.append("Accept-Language: en").unwrap();
    easy.http_headers(headers).unwrap();

    match easy.perform() {
        Err(_) => { return None; }
        _      => ()
    }

    let contents = easy.get_ref();

    let s = String::from_utf8_lossy(&contents.0).to_string();

    parse_content(&s)
}

fn parse_content(page_contents: &String) -> Option<String> {

    let s1: Vec<_> = page_contents.split("<title>").collect();
    if s1.len() < 2 { return None }
    let s2: Vec<_> = s1[1].split("</title>").collect();
    if s2.len() < 2 { return None }

    let title_enc = s2[0];

    let mut title_dec = String::new();
    match decode_html(title_enc) {
        Ok(s) => { title_dec = s; }
        _     => ()
    };

    /* strip leading and tailing whitespace from title */
    let re = Regex::new(r"^[\s\n]*(?P<title>.*?)[\s\n]*$").unwrap();
    /* TODO: use trim https://doc.rust-lang.org/std/string/struct.String.html#method.trim */
    let res = re.captures(&title_dec).unwrap()["title"].to_string();

    match res.chars().count() {
        0 => None,
        _ => {
            println!("SUCCESS \"{}\"", res);
            Some(res)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_urls() {
        assert_ne!(None, resolve_url("https://youtube.com"));
        assert_ne!(None, resolve_url("https://google.co.uk"));
    }

    #[test]
    fn parse_contents() {
        assert_eq!(None, parse_content(&"".to_string()));
        assert_eq!(None, parse_content(&"    ".to_string()));
        assert_eq!(None, parse_content(&"<title></title>".to_string()));
        assert_eq!(None, parse_content(&"<title>    </title>".to_string()));
        assert_eq!(None, parse_content(&"floofynips, not a real webpage".to_string()));
        assert_eq!(Some("cheese is nice".to_string()), parse_content(&"<title>cheese is nice</title>".to_string()));
        assert_eq!(Some("squanch".to_string()), parse_content(&"<title>     squanch</title>".to_string()));
        assert_eq!(Some("squanch".to_string()), parse_content(&"<title>squanch     </title>".to_string()));
        assert_eq!(Some("squanch".to_string()), parse_content(&"<title>\nsquanch</title>".to_string()));
        assert_eq!(Some("squanch".to_string()), parse_content(&"<title>\n  \n  squanch</title>".to_string()));
        assert_eq!(Some("we like the moon".to_string()), parse_content(&"<title>\n  \n  we like the moon</title>".to_string()));
        assert_eq!(Some("&hello123&<>''~".to_string()), parse_content(&"<title>&amp;hello123&amp;&lt;&gt;''~</title>".to_string()));
    }
}
