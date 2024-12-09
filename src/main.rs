use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Song {
    id: u32,
    title: String,
    artist: String,
    genre: String,
    play_count: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct NewSong {
    title: String,
    artist: String,
    genre: String,
}

fn main() -> Result<()> {
    // Initialize SQLite database and reset it
    let db_connection = Arc::new(Mutex::new(init_and_reset_database()?));
    let visit_count = Arc::new(Mutex::new(0));
    // Bind the server to localhost:8080
    let listener = TcpListener::bind("127.0.0.1:8080").expect("Failed to bind to port 8080");
    println!("The server is currently listening on localhost:8080.");

    // Handle incoming connections
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let visit_count = Arc::clone(&visit_count);
                let db_connection = Arc::clone(&db_connection);
                thread::spawn(move || {
                    handle_client(stream, visit_count, db_connection);
                });
            }
            Err(e) => eprintln!("Failed to accept connection: {}", e),
        }
    }

    Ok(())
}

fn init_and_reset_database() -> Result<Connection> {
    let db_connection = Connection::open("songs.db")?;

    // Create the songs' table
    db_connection.execute(
        "CREATE TABLE IF NOT EXISTS songs (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            title       TEXT NOT NULL,
            artist      TEXT NOT NULL,
            genre       TEXT NOT NULL,
            play_count  INTEGER NOT NULL DEFAULT 0
        )",
        [],
    )?;

    // Reset the database
    db_connection.execute("DELETE FROM songs", [])?;
    // Reset the counter
    db_connection.execute("DELETE FROM sqlite_sequence WHERE name = 'songs'", [])?;
    Ok(db_connection)
}

fn handle_client(
    mut stream: TcpStream,
    visit_count: Arc<Mutex<u32>>,
    db_connection: Arc<Mutex<Connection>>,
) {
    let mut buffer = [0; 1024];
    if let Err(e) = stream.read(&mut buffer) {
        println!("Failed to read from client: {}", e);
        return;
    }

    let request = String::from_utf8_lossy(&buffer);

    // Extract header and body
    let headers_end = request.find("\r\n\r\n").unwrap_or(request.len());
    let headers = &request[..headers_end];
    let body = &request[headers_end + 4..];

    // Check for content type
    let is_json_request = headers.contains("Content-Type: application/json");

    if request.starts_with("GET /count ") {
        let mut count = visit_count.lock().unwrap();
        *count += 1;

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nVisit count: {}",
            count
        );

        if let Err(e) = stream.write_all(response.as_bytes()) {
            println!("Failed to write response to client: {}", e);
        }
    } else if request.starts_with("POST /songs/new ") {
        // Handle new song creation
        if is_json_request {
            let content_length_header = headers
                .lines()
                .find(|line| line.starts_with("Content-Length:"))
                .and_then(|line| line.split(": ").nth(1))
                .and_then(|value| value.trim().parse::<usize>().ok());

            if let Some(content_length) = content_length_header {
                // Get the exact body
                let body = &body[..content_length];

                match serde_json::from_str::<NewSong>(body) {
                    Ok(new_song) => {
                        let db_connection = db_connection.lock().unwrap();
                        if let Err(e) = db_connection.execute(
                            "INSERT INTO songs (title, artist, genre, play_count) VALUES (?1, ?2, ?3, ?4)",
                            params![new_song.title, new_song.artist, new_song.genre, 0],
                        ) {
                            println!("Failed to insert song: {}", e);
                            let response = "HTTP/1.1 500 Internal Server Error\r\n\r\n";
                            let _ = stream.write_all(response.as_bytes());
                            return;
                        }

                        // Retrieve the auto-generated `id` of the inserted song
                        let song_id: u32 = db_connection.last_insert_rowid() as u32;
                        let song = Song {
                            id: song_id,
                            title: new_song.title,
                            artist: new_song.artist,
                            genre: new_song.genre,
                            play_count: 0,
                        };

                        let response_body = serde_json::to_string(&song).unwrap();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{}",
                            response_body
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(e) => {
                        println!("Failed to parse JSON: {}", e);
                        let response = "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\n\r\nInvalid JSON format.";
                        let _ = stream.write_all(response.as_bytes());
                    }
                }
            } else {
                let response = "HTTP/1.1 411 Length Required\r\nContent-Type: text/plain\r\n\r\nMissing Content-Length header.";
                let _ = stream.write_all(response.as_bytes());
            }
        } else {
            let response = "HTTP/1.1 415 Unsupported Media Type\r\nContent-Type: text/plain\r\n\r\nExpected Content-Type: application/json.";
            let _ = stream.write_all(response.as_bytes());
        }
    } else if request.starts_with("GET /songs/search?") {
        // Song search functionality
        let query = request
            .split_once("GET /songs/search?")
            .unwrap()
            .1
            .split(" ")
            .next()
            .unwrap_or("");

        let params: Vec<(String, String)> = query
            .split('&')
            .filter_map(|pair| {
                let mut parts = pair.split('=');
                Some((
                    parts.next()?.to_lowercase(),
                    parts.next()?.replace("+", " ").to_lowercase(),
                ))
            })
            .collect();

        let mut conditions = String::new();
        let mut arguments = Vec::new();

        for (key, value) in params {
            match key.as_str() {
                "title" => {
                    conditions.push_str(" AND title LIKE ?");
                    arguments.push(format!("%{}%", value));
                }
                "artist" => {
                    conditions.push_str(" AND artist LIKE ?");
                    arguments.push(format!("%{}%", value));
                }
                "genre" => {
                    conditions.push_str(" AND genre LIKE ?");
                    arguments.push(format!("%{}%", value));
                }
                _ => {}
            }
        }

        let final_query = format!(
            "SELECT id, title, artist, genre, play_count FROM songs WHERE 1=1{}",
            conditions
        );

        let sql_params: Vec<&dyn rusqlite::ToSql> = arguments
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();
        let db_connection = db_connection.lock().unwrap();
        let mut prepared_statement = db_connection.prepare(&final_query).unwrap();

        let song_iter = prepared_statement.query_map(sql_params.as_slice(), |row| {
            Ok(Song {
                id: row.get(0)?,
                title: row.get(1)?,
                artist: row.get(2)?,
                genre: row.get(3)?,
                play_count: row.get(4)?,
            })
        });

        match song_iter {
            Ok(results) => {
                let songs: Vec<Song> = results.filter_map(Result::ok).collect();
                let response_body = serde_json::to_string(&songs).unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{}",
                    response_body
                );
                let _ = stream.write_all(response.as_bytes());
            }
            Err(e) => {
                eprintln!("Failed to query songs: {}", e);
                let response =
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\n\r\n";
                let _ = stream.write_all(response.as_bytes());
            }
        }
    } else if request.starts_with("GET /songs/play/") {
        // Play song functionality
        let id_str = request
            .split("/songs/play/")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap_or("");
        if let Ok(id) = id_str.parse::<u32>() {
            let db_connection = db_connection.lock().unwrap();
            let mut prepared_statement = db_connection
                .prepare("SELECT id, title, artist, genre, play_count FROM songs WHERE id = ?1")
                .unwrap();
            let song: Result<Song> = prepared_statement.query_row([id], |row| {
                Ok(Song {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    artist: row.get(2)?,
                    genre: row.get(3)?,
                    play_count: row.get(4)?,
                })
            });

            match song {
                Ok(mut song) => {
                    song.play_count += 1;
                    db_connection
                        .execute(
                            "UPDATE songs SET play_count = ?1 WHERE id = ?2",
                            params![song.play_count, song.id],
                        )
                        .unwrap();

                    let response_body = serde_json::to_string(&song).unwrap();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{}",
                        response_body
                    );

                    let _ = stream.write_all(response.as_bytes());
                }
                Err(_) => {
                    let response = "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\r\n{\"error\":\"Song not found\"}";
                    let _ = stream.write_all(response.as_bytes());
                }
            }
        } else {
            let response =
                "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\n\r\nInvalid song ID.";
            let _ = stream.write_all(response.as_bytes());
        }
    } else {
        let response = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nWelcome to the Rust-powered web server!";
        if let Err(e) = stream.write_all(response) {
            eprintln!("Failed to write response to client: {}", e);
        }
    }
}
