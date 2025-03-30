use futures::future::join_all;
use indicatif::{ProgressBar, ProgressStyle};
use percent_encoding::percent_decode;
use reqwest::header::CONTENT_LENGTH;
use reqwest::header::RANGE;
use reqwest::Url;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn extract_filename(url: &str) -> String {
    let parsed = Url::parse(url).ok();
    let path = parsed.as_ref().and_then(|u| Some(u.path()));

    let (base, ext) = path
        .map(|p| {
            let path = Path::new(p);
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("bin");
            (stem, ext)
        })
        .unwrap_or_else(|| ("download", "bin"));

    let host_name = parsed
        .as_ref()
        .and_then(|u| u.host_str())
        .unwrap_or("download");

    let safe_name = percent_decode(base.as_bytes())
        .decode_utf8_lossy()
        .replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "_")
        .trim_end_matches('_')
        .to_string();

    if safe_name.is_empty() {
        format!("{}.{}", host_name.replace('.', "_"), ext)
    } else {
        format!("{}.{}", safe_name, ext)
    }
}
#[tokio::main]
async fn main() {
    println!("Please input url:");
    let mut url = String::new();
    io::stdin().read_line(&mut url).expect("Read line failed");
    let url = url.trim();

    let client = reqwest::Client::new();
    let head_response = client
        .head(url)
        .send()
        .await
        .expect("Send head request failed");

    let accept_ranges = head_response
        .headers()
        .get("accept-ranges")
        .and_then(|v| v.to_str().ok());

    if accept_ranges == Some("bytes") {
        let content_length_header = head_response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("0");
        let content_length: u64 = content_length_header
            .parse()
            .expect("Invalid content length");
        let chunk_count = 500.min(content_length);
        println!("Will split into {} chunks", chunk_count);
        let chunk_size = content_length / chunk_count;
        let mut tasks = Vec::new();
        let filename = extract_filename(url);
        println!("File will save to : {}", filename);
        let temp_dir = tempfile::tempdir().expect("Create temp dir failed");
        let temp_files: Vec<PathBuf> = Vec::new();

        let pb = Arc::new(ProgressBar::new(content_length));
        pb.set_style(ProgressStyle::default_bar()
             .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
             .unwrap()
             .progress_chars("#>-"));

        for i in 0..chunk_count {
            let start = i * chunk_size;
            let end = if i == chunk_count - 1 {
                content_length - 1
            } else {
                (i + 1) * chunk_size - 1
            };
            let client = client.clone();
            let url = url.to_string();
            let temp_path = temp_dir.path().join(format!("part{}", i));
            let pb = pb.clone();
            tasks.push(tokio::spawn(async move {
                let response = client
                    .get(&url)
                    .header(RANGE, format!("bytes={}-{}", start, end))
                    .send()
                    .await
                    .expect("GET request failed");
                let mut file = File::create(&temp_path)
                    .await
                    .expect("Create temp file failed");
                let bytes = response.bytes().await.expect("Read bytes failed");
                file.write_all(&bytes)
                    .await
                    .expect("write temp file failed");
                pb.inc(bytes.len() as u64);
            }));
        }

        join_all(tasks).await;

        let mut file = File::create(&filename).await.expect("Create file failed");
        for temp_path in temp_files {
            let mut temp_file = File::open(&temp_path).await.expect("Open temp file failed");
            let mut buffer = Vec::new();
            temp_file
                .read_to_end(&mut buffer)
                .await
                .expect("Read temp file failed");
            file.write_all(&buffer).await.expect("Write file failed");
        }

        temp_dir.close().expect("Remove temp dir failed");
        println!("Download complete!");
        pb.finish_with_message("Download complete");
    } else {
        println!("Server does not support range requests");
    }
}
