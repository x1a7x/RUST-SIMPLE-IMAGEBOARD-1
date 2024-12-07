
use actix_files as fs;
use actix_multipart::Multipart;
use actix_web::{
    web, App, HttpResponse, HttpServer, Responder, middleware, Error,
};
use askama::Template;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sled::Db;
use std::sync::Arc;
use log::{error, info};
use futures_util::stream::StreamExt;
use std::io::Write;
use uuid::Uuid;

const UPLOAD_DIR: &str = "./uploads/";
const THUMB_DIR: &str = "./thumbs/";

#[derive(Template)]
#[template(path = "homepage.html")]
struct HomepageTemplate<'a> {
    threads: &'a [Thread],
    current_page: i32,
    total_pages: i32,
}

#[derive(Template)]
#[template(path = "thread.html")]
struct ThreadTemplate<'a> {
    thread: &'a Thread,
    replies: &'a [Reply],
}

#[derive(Serialize, Deserialize, Clone)]
struct Thread {
    id: i32,
    title: String,
    message: String,
    last_updated: i64, // Unix timestamp
    image_url: Option<String>, // Image URL for threads
}

#[derive(Serialize, Deserialize)]
struct Reply {
    id: i32,
    message: String,
}

#[derive(Deserialize)]
struct PaginationParams {
    page: Option<i32>,
}

#[derive(Deserialize)]
struct ReplyForm {
    parent_id: i32,
    message: String,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();

    // Ensure directories exist
    for dir in &[UPLOAD_DIR, THUMB_DIR] {
        if !std::path::Path::new(dir).exists() {
            std::fs::create_dir_all(dir).unwrap();
            info!("Created directory: {}", dir);
        }
    }

    // Initialize sled database
    let sled_db = Arc::new(sled::open("sled_db").expect("Failed to open sled database"));

    // Start Actix server
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(sled_db.clone()))
            .wrap(middleware::Logger::default())
            .service(fs::Files::new("/static", "./static").show_files_listing())
            .service(fs::Files::new("/uploads", UPLOAD_DIR).show_files_listing()) // Serve uploaded images
            .route("/", web::get().to(homepage))
            .route("/thread/{id}", web::get().to(view_thread))
            .route("/thread", web::post().to(create_thread))
            .route("/reply", web::post().to(create_reply))
    })
    .bind(("0.0.0.0", 8080))?
    .run()
    .await
}

// Homepage handler
async fn homepage(
    db: web::Data<Arc<Db>>,
    query: web::Query<PaginationParams>,
) -> impl Responder {
    let page_size = 10;
    let page_number = query.page.unwrap_or(1);

    let mut threads = get_all_threads(&db);
    threads.sort_by(|a, b| b.last_updated.cmp(&a.last_updated));

    let total_threads = threads.len() as i32;
    let total_pages = (total_threads as f64 / page_size as f64).ceil() as i32;

    let page_number = if page_number < 1 {
        1
    } else if page_number > total_pages && total_pages > 0 {
        total_pages
    } else {
        page_number
    };

    let start_index = ((page_number - 1) * page_size) as usize;
    let end_index = (start_index + page_size as usize).min(threads.len());
    let threads = &threads[start_index..end_index];

    let tmpl = HomepageTemplate {
        threads,
        current_page: page_number,
        total_pages,
    };

    match tmpl.render() {
        Ok(rendered) => HttpResponse::Ok().content_type("text/html").body(rendered),
        Err(e) => {
            error!("Template rendering error: {}", e);
            HttpResponse::InternalServerError().body("Error rendering page")
        }
    }
}

// Fetch all threads from sled
fn get_all_threads(db: &Db) -> Vec<Thread> {
    db.scan_prefix(b"thread_")
        .filter_map(|res| {
            if let Ok((_, value)) = res {
                serde_json::from_slice(&value).ok()
            } else {
                None
            }
        })
        .collect()
}

// Count total number of threads in sled
fn count_threads(db: &Db) -> i32 {
    db.scan_prefix(b"thread_").count() as i32
}

// Thread viewing handler
async fn view_thread(
    db: web::Data<Arc<Db>>,
    path: web::Path<(i32,)>,
) -> impl Responder {
    let thread_id = path.into_inner().0;
    let thread_key = format!("thread_{}", thread_id).into_bytes();
    let thread: Option<Thread> = db.get(&thread_key).ok().flatten().and_then(|value| {
        serde_json::from_slice(&value).ok()
    });

    if thread.is_none() {
        return HttpResponse::NotFound().body("Thread not found");
    }

    let thread = thread.unwrap();
    let replies = get_replies(&db, thread_id);

    let tmpl = ThreadTemplate {
        thread: &thread,
        replies: &replies,
    };

    match tmpl.render() {
        Ok(rendered) => HttpResponse::Ok().content_type("text/html").body(rendered),
        Err(e) => {
            error!("Template rendering error: {}", e);
            HttpResponse::InternalServerError().body("Error rendering page")
        }
    }
}

// Create thread handler with image upload
async fn create_thread(
    db: web::Data<Arc<Db>>,
    mut payload: Multipart,
) -> Result<HttpResponse, Error> {
    let mut title = String::new();
    let mut message = String::new();
    let mut image_url: Option<String> = None;

    while let Some(item) = payload.next().await {
        let mut field = item?;
        let content_disposition = field.content_disposition();

        let name = if let Some(name) = content_disposition.get_name() {
            name
        } else {
            continue;
        };

        match name {
            "title" => {
                while let Some(chunk) = field.next().await {
                    let data = chunk?;
                    title.push_str(&String::from_utf8_lossy(&data));
                }
            }
            "message" => {
                while let Some(chunk) = field.next().await {
                    let data = chunk?;
                    message.push_str(&String::from_utf8_lossy(&data));
                }
            }
            "image" => {
                // Handle image upload
                if let Some(filename) = content_disposition.get_filename() {
                    // Skip processing if filename is empty
                    if filename.trim().is_empty() {
                        continue;
                    }

                    // Validate file extension
                    if !filename.to_lowercase().ends_with(".jpg") && !filename.to_lowercase().ends_with(".jpeg") {
                        return Ok(HttpResponse::BadRequest().body("Only JPEG images are allowed"));
                    }

                    // Generate a unique filename
                    let unique_id = Uuid::new_v4().to_string();
                    let sanitized_filename = format!("{}.jpg", unique_id);
                    let filepath = format!("{}{}", UPLOAD_DIR, sanitized_filename);

                    // Save the file with a move closure to capture ownership
                    let mut f = web::block(move || std::fs::File::create(&filepath)).await??;

                    while let Some(chunk) = field.next().await {
                        let data = chunk?;
                        f = web::block(move || f.write_all(&data).map(|_| f)).await??;
                    }

                    image_url = Some(format!("/uploads/{}", sanitized_filename));
                }
            }
            _ => {}
        }
    }

    // Ensure that title and message are not empty
    if title.trim().is_empty() || message.trim().is_empty() {
        return Ok(HttpResponse::BadRequest().body("Title and Message cannot be empty"));
    }

    let thread_id = count_threads(&db) + 1;
    let thread = Thread {
        id: thread_id,
        title: title.trim().to_string(),
        message: message.trim().to_string(),
        last_updated: Utc::now().timestamp(),
        image_url,
    };

    let key = format!("thread_{}", thread_id).into_bytes();
    let value = serde_json::to_vec(&thread).expect("Failed to serialize thread");

    if db.insert(key, value).is_ok() {
        Ok(HttpResponse::SeeOther()
            .append_header(("Location", "/"))
            .finish())
    } else {
        error!("Failed to insert thread into sled db");
        Ok(HttpResponse::InternalServerError().body("Failed to create thread"))
    }
}

// Create reply handler without image upload
async fn create_reply(
    db: web::Data<Arc<Db>>,
    form: web::Form<ReplyForm>,
) -> Result<HttpResponse, Error> {
    let parent_id = form.parent_id;
    let message = form.message.trim().to_string();

    // Ensure that message is not empty
    if message.is_empty() {
        return Ok(HttpResponse::BadRequest().body("Message cannot be empty"));
    }

    let reply_id = count_replies(&db, parent_id) + 1;
    let reply = Reply {
        id: reply_id,
        message,
    };

    let key = format!("reply_{}_{}", parent_id, reply_id).into_bytes();
    let value = serde_json::to_vec(&reply).expect("Failed to serialize reply");

    if db.insert(key, value).is_ok() {
        // Update thread's last_updated
        let thread_key = format!("thread_{}", parent_id).into_bytes();
        if let Some(thread_bytes) = db.get(&thread_key).ok().flatten() {
            if let Ok(mut thread) = serde_json::from_slice::<Thread>(&thread_bytes) {
                thread.last_updated = Utc::now().timestamp();
                let updated = serde_json::to_vec(&thread).expect("Failed to serialize updated thread");
                db.insert(thread_key, updated).ok();
            }
        }

        Ok(HttpResponse::SeeOther()
            .append_header(("Location", format!("/thread/{}", parent_id)))
            .finish())
    } else {
        error!("Failed to insert reply into sled db");
        Ok(HttpResponse::InternalServerError().body("Failed to post reply"))
    }
}

// Fetch replies for a thread from sled
fn get_replies(db: &Db, parent_id: i32) -> Vec<Reply> {
    db.scan_prefix(format!("reply_{}", parent_id).as_bytes())
        .filter_map(|res| {
            if let Ok((_, value)) = res {
                serde_json::from_slice(&value).ok()
            } else {
                None
            }
        })
        .collect::<Vec<Reply>>()
}

// Count total number of replies for a thread in sled
fn count_replies(db: &Db, parent_id: i32) -> i32 {
    db.scan_prefix(format!("reply_{}", parent_id).as_bytes()).count() as i32
}
