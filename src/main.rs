use anyhow::Context;
use rand::Rng;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use axum::{
    Json, Router,
    extract::{Path, Query},
    http::{Method, StatusCode},
    routing::{get, post},
};
use serde_rusqlite::from_row;
use tower_http::trace::TraceLayer;
use tower_http::{
    cors::{Any, CorsLayer},
    trace::DefaultOnRequest,
};
use tracing::Level;

mod ratings;

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
        // TODO: We manually set the target time here, but it should be set in the database
        let num_pieces = row
            .root_tps
            .chars()
            .filter(|c| *c == '1' || *c == '2')
            .count()
            / 2;
        let length = row.solution.split_whitespace().count().div_ceil(2);
        let low_target_time = (20.0 + num_pieces as f32) * length as f32;
        let target_time = rand::rng().random_range(low_target_time..(low_target_time * 1.2)) as u32;
        Self {
            id: row.id,
            size: row.size,
            komi: row.komi,
            root_tps: row.root_tps,
            defender_start_move: row.defender_start_move,
            solution: row.solution.split_whitespace().map(String::from).collect(),
            target_time_seconds: target_time,
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
        .route("/puzzles/{id}/rating", get(get_puzzle_rating))
        .route("/puzzles", get(get_puzzle))
        .route("/puzzles/{id}", post(solve_puzzle))
        .layer(
            CorsLayer::new()
                .allow_methods([Method::GET, Method::POST])
                .allow_headers(Any)
                .allow_origin(Any),
        )
        .layer(tower::ServiceBuilder::new().layer(
            TraceLayer::new_for_http().on_request(DefaultOnRequest::new().level(Level::INFO)),
        ));

    // run our app with hyper, listening globally on port 3000
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Listening on http://{}", listener.local_addr().unwrap());
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

    // Ratings have to be inserted manually for now
    db_conn.execute(
        "CREATE TABLE IF NOT EXISTS \"users\" (
	    \"username\" TEXT NOT NULL,
	    \"rating\" REAL NOT NULL,
	    PRIMARY KEY(\"username\")
    )",
        [],
    )?;

    Ok(())
}

// Get a random puzzle
#[axum::debug_handler]
async fn get_puzzle(username: Query<PuzzleRequest>) -> Result<Json<Puzzle>, StatusCode> {
    if username.username.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let db_conn = Connection::open("puzzles.db")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
        .unwrap();
    let puzzles_solved = read_puzzle_attempts_for_user(&db_conn, &username.username)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Always show puzzle 3 first
    if !puzzles_solved.iter().any(|attempt| attempt.puzzle_id == 3) {
        let puzzle_3 = read_puzzle_by_id(&db_conn, 3)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .unwrap();
        return Ok(Json(Puzzle::from(puzzle_3)));
    }

    // Always show puzzle 15 second
    if !puzzles_solved.iter().any(|attempt| attempt.puzzle_id == 15) {
        let puzzle_15 = read_puzzle_by_id(&db_conn, 15)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .unwrap();
        return Ok(Json(Puzzle::from(puzzle_15)));
    }

    // Then show any puzzle up to id 20
    match read_unsolved_puzzles_from_db(&db_conn, &username.username) {
        Ok(Some(puzzle)) => Ok(Json(puzzle.into())),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            eprintln!("Error reading puzzles from database: {:?}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

// Get elo rating of a single puzzle
// Depends on player ratings being manually added to the `users` table
async fn get_puzzle_rating(Path(id): Path<u32>) -> Result<Json<f64>, StatusCode> {
    let db_conn = Connection::open("puzzles.db")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
        .unwrap();
    let rating = ratings::rating_for_puzzles(&db_conn, id as i64)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(rating.rating))
}

// Solve puzzle
#[axum::debug_handler]
async fn solve_puzzle(
    Path(id): Path<u32>,
    Json(payload): Json<PuzzleResponse>,
) -> Result<(), StatusCode> {
    if payload.username.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
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
        WHERE puzzles.id < 20 AND puzzle_attempts.puzzle_id IS NULL ORDER BY RANDOM() LIMIT 1",
    )?;
    Ok(stmt
        .query_and_then([username], from_row::<PuzzleRow>)?
        .next()
        .transpose()?)
}

fn read_puzzle_attempts_for_user(
    db_conn: &Connection,
    username: &str,
) -> anyhow::Result<Vec<PuzzleAttemptRow>> {
    let mut stmt = db_conn.prepare(
        "WITH ranked_attempts AS (
            SELECT *,
                ROW_NUMBER() OVER (
                    PARTITION BY username, puzzle_id
                    ORDER BY timestamp_seconds ASC
                ) AS rn
            FROM puzzle_attempts WHERE username = ?1
        )
        SELECT *
        FROM ranked_attempts
        WHERE rn = 1;
        ",
    )?;
    let rows = stmt.query_and_then([username], from_row::<PuzzleAttemptRow>)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn read_puzzle_by_id(db_conn: &Connection, id: u32) -> anyhow::Result<Option<PuzzleRow>> {
    let mut stmt = db_conn.prepare("SELECT * FROM puzzles WHERE id = ?1")?;
    Ok(stmt
        .query_and_then([id], from_row::<PuzzleRow>)?
        .next()
        .transpose()?)
}
