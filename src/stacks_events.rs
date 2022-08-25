use blockstack_lib::chainstate::stacks::StacksTransaction;
use chrono::{DateTime, Local, SecondsFormat, Utc};
use lazy_static::lazy_static;
use regex::Regex;
use serde_json::{json, Value};
use stacks_common::codec::StacksMessageCodec;
use stacks_common::util::hash::hex_bytes;
use std::fs::File;
use std::io::prelude::*;
use std::io::BufReader;
use std::net::TcpListener;
use std::net::TcpStream;
use std::time::SystemTime;
use std::{env, io};

const DEFAULT_ADDR: &str = "127.0.0.1:3700";

fn main() {
    let mut args = std::fmt::Arguments::from_env();
    let addr = args.opt_value_from_str("--addr").unwrap_or(DEFAULT_ADDR);
    let help = args.opt_value_from_str("--help").unwrap_or(false);

    if help {
        println!("Usage: stacks-events [--addr=<addr>]");
        println!(
            "  --addr=<addr>  Address to listen on (default: {})",
            DEFAULT_ADDR
        );
        return;
    }

    serve_for_events(addr);
}

fn serve_for_events(addr: &str) {
    let listener = TcpListener::bind(addr).unwrap();
    eprintln!("Listening on {}", addr);
    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        handle_connection(stream);
    }
}

lazy_static! {
    static ref RE_POST: Regex = Regex::new(r"^POST /(.*?) HTTP/1.1\r\n$").unwrap();
    static ref RE_CONTENT_LENGTH: Regex = Regex::new(r"^content-length: (\d+)\r\n$").unwrap();
}

fn handle_connection(mut stream: TcpStream) {
    let mut buf = String::with_capacity(10 * 1024);
    let mut reader = BufReader::new(stream.try_clone().unwrap());

    let mut path = None;
    let mut content_length = None;
    let mut payload = None;

    loop {
        buf.clear();
        reader.read_line(&mut buf).unwrap();
        if path.is_none() {
            let caps = RE_POST.captures(&buf).unwrap();
            path = Some(caps.get(1).unwrap().as_str().to_string());
        } else if content_length.is_none() {
            let caps = RE_CONTENT_LENGTH.captures(&buf);
            if let Some(caps) = caps {
                content_length = Some(caps.get(1).unwrap().as_str().parse::<u64>().unwrap());
            }
        } else if buf == "\r\n" {
            buf.clear();
            reader
                .take(content_length.unwrap())
                .read_to_string(&mut buf)
                .unwrap();
            payload = Some(buf.to_owned());
            break;
        }
    }

    let payload_json: Value = serde_json::from_str(&payload.unwrap()).unwrap();
    let record = json!({
        "ts": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        "path": path.unwrap(),
        "payload": payload_json,
    });
    println!("{}", record);

    {
        let contents = "Thanks!";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            contents.len(),
            contents
        );

        stream.write(response.as_bytes()).unwrap();
        stream.flush().unwrap();
    }
}
