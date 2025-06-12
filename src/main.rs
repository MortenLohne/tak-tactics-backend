use anyhow::Context;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use axum::{
    Json, Router,
    extract::{Path, Query},
    http::{HeaderValue, Method, StatusCode},
    routing::{get, post},
};
use serde_rusqlite::from_row;
use tower_http::cors::CorsLayer;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Puzzle {
    id: u64,
    size: usize,
    komi: String,
    #[serde(rename = "rootTPS")]
    root_tps: String,
    defender_start_move: String,
    solution: Vec<String>,
    target_time_seconds: u32,
    player_white: String,
    player_black: String,
    playtak_game_id: usize,
}

impl From<PuzzleRow> for Puzzle {
    fn from(row: PuzzleRow) -> Self {
        Self {
            id: row.id,
            size: row.size,
            komi: row.komi,
            root_tps: row.root_tps,
            defender_start_move: row.defender_start_move,
            solution: row.solution.split_whitespace().map(String::from).collect(),
            target_time_seconds: row.target_time_seconds,
            player_white: row.player_white,
            player_black: row.player_black,
            playtak_game_id: row.playtak_game_id,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct PuzzleRequest {
    username: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PuzzleResponse {
    id: usize,
    username: String,
    solved: bool,
    solution: Vec<String>,
    solve_time_seconds: u32,
}

#[tokio::main]
async fn main() {
    // initialize tracing
    tracing_subscriber::fmt::init();

    init_db_tables().unwrap();

    // build our application with a route
    let app = Router::new()
        // `GET /` goes to `root`
        .route("/puzzles", get(get_puzzle))
        .layer(
            CorsLayer::new()
                .allow_methods([Method::GET, Method::POST])
                .allow_origin("http://localhost:8000".parse::<HeaderValue>().unwrap()),
        )
        .route("/puzzles/{*id}", post(solve_puzzle));

    // run our app with hyper, listening globally on port 3000
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

pub fn init_db_tables() -> anyhow::Result<()> {
    let db_conn = Connection::open("puzzles.db").context("Failed to open database connection")?;

    db_conn.execute(
        "CREATE TABLE IF NOT EXISTS puzzles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            root_tps TEXT NOT NULL,
            defender_start_move TEXT NOT NULL,
            size INTEGER NOT NULL,
            komi TEXT NOT NULL,
            player_white TEXT NOT NULL,
            player_black TEXT NOT NULL,
            solution TEXT NOT NULL,
            initial_rating INTEGER,
            rating INTEGER,
            target_time_seconds INTEGER NOT NULL DEFAULT 60,
            playtak_game_id INTEGER NOT NULL
        )",
        [],
    )?;

    db_conn.execute(
        "CREATE TABLE IF NOT EXISTS puzzle_attempts (
            puzzle_id INTEGER NOT NULL,
            username TEXT NOT NULL,
            solved INTEGER NOT NULL,
            solve_time_seconds INTEGER NOT NULL,
            solution TEXT NOT NULL,
            timestamp_seconds INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            FOREIGN KEY (puzzle_id) REFERENCES puzzles(id)
        )",
        [],
    )?;

    Ok(())
}

// Get a random puzzle
#[axum::debug_handler]
async fn get_puzzle(username: Query<PuzzleRequest>) -> Result<Json<Puzzle>, StatusCode> {
    let db_conn = Connection::open("puzzles.db")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
        .unwrap();
    match read_unsolved_puzzles_from_db(&db_conn, &username.username) {
        Ok(Some(puzzle)) => Ok(Json(puzzle.into())),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            eprintln!("Error reading puzzles from database: {:?}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

// Solve puzzle
#[axum::debug_handler]
async fn solve_puzzle(
    Path(id): Path<u32>,
    Json(payload): Json<PuzzleResponse>,
) -> Result<(), StatusCode> {
    let db_conn = Connection::open("puzzles.db").map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    db_conn
        .execute(
            "INSERT INTO puzzle_attempts (puzzle_id, username, solved, solve_time_seconds, solution)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                id,
                payload.username,
                payload.solved,
                payload.solve_time_seconds,
                payload.solution.join(" ")
            ],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(())
}

// INSERT INTO puzzles (size, komi, root_tps, defender_start_move, solution, target_time_seconds, player_white, player_black, playtak_game_id)
// VALUES (6, "2", "2,x,x,2,1,1/2,x,2,2,1,2S/2222221S,x,x,121C,1,x/x,112,11112C,2,21211112S,2/2,22221S,2,1,x,1/2,x,1,1,1,1 2 47", "5e3< d4- 3e3+12 *", 120, "x57696c6c", "EVRNjayhawker", 491458)

#[derive(Serialize, Deserialize)]
struct PuzzleRow {
    id: u64,
    root_tps: String,
    defender_start_move: String,
    size: usize,
    komi: String,
    player_white: String,
    player_black: String,
    solution: String,
    initial_rating: Option<i32>,
    rating: Option<i32>,
    target_time_seconds: u32,
    playtak_game_id: usize,
}

#[derive(Serialize, Deserialize)]
struct PuzzleAttemptRow {
    puzzle_id: u64,
    username: String,
    solved: bool,
    solve_time_seconds: u32,
    solution: String,
    timestamp_seconds: u64,
}

fn read_unsolved_puzzles_from_db(
    db_conn: &Connection,
    username: &str,
) -> anyhow::Result<Option<PuzzleRow>> {
    let mut stmt = db_conn.prepare(
        "SELECT puzzles.* FROM puzzles
        LEFT JOIN puzzle_attempts ON puzzles.id = puzzle_attempts.puzzle_id AND puzzle_attempts.username = ?1
        WHERE puzzle_attempts.puzzle_id IS NULL ORDER BY RANDOM() LIMIT 1",
    )?;
    Ok(stmt
        .query_and_then([username], from_row::<PuzzleRow>)?
        .next()
        .transpose()?)
}
