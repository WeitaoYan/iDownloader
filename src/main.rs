use clap::Parser;
use futures::future::join_all;
use indicatif::{ProgressBar, ProgressStyle};
use percent_encoding::percent_decode;
use reqwest::header::CONTENT_LENGTH;
use reqwest::header::RANGE;
use reqwest::Url;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// URL to download
    url: String,

    /// Output directory
    #[arg(short, long, value_name = "DIR")]
    output: Option<PathBuf>,

    /// Maximum number of chunks
    #[arg(short, long, default_value_t = 500, value_name = "NUM")]
    max_chunks: u64,

    /// Maximum number of retries
    #[arg(short = 'r', long, default_value_t = 3, value_name = "NUM")]
    max_retries: u64, // 添加最大重试次数参数
}
fn extract_filename(url: &str, headers: &reqwest::header::HeaderMap) -> String {
    // 首先尝试从 Content-Disposition 头中获取文件名
    if let Some(content_disposition) = headers.get("content-disposition") {
        if let Ok(content_disposition_str) = content_disposition.to_str() {
            if let Some(filename) = content_disposition_str
                .split(';')
                .find_map(|part| part.trim().strip_prefix("filename="))
            {
                return filename.to_string();
            }
        }
    }

    // 如果没有找到，再从URL路径中提取文件名
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
    let args = Args::parse();

    let url = args.url.trim();

    let client = reqwest::Client::new();
    let head_response = client
        .head(url)
        .send()
        .await
        .expect("Send head request failed");

    if !head_response.status().is_success() {
        eprintln!(
            "Failed to download: Server returned status code {}",
            head_response.status()
        );
        return;
    }
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
        let chunk_count = args.max_chunks.min(content_length as u64);
        println!("Will split into {} chunks", chunk_count);
        let chunk_size = content_length / chunk_count;
        let mut tasks = Vec::new();
        let filename = extract_filename(url, &head_response.headers());

        // Determine the output directory
        let output_dir = args.output.unwrap_or_else(|| {
            let home_dir = dirs::download_dir().expect("Failed to get download directory");
            home_dir
        });
        let file_path = output_dir.join(&filename);

        let temp_dir = tempdir().expect("Create temp dir failed");
        let temp_files: Arc<Mutex<Vec<Option<PathBuf>>>> =
            Arc::new(Mutex::new(vec![None; chunk_count as usize]));
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
            let temp_files_clone = temp_files.clone(); // 克隆 Arc<Mutex>
            let index = i as usize;
            let max_retries = args.max_retries; // 获取最大重试次数
            tasks.push(tokio::spawn(async move {
                let mut retries = 0;
                while retries < max_retries {
                    // 使用新参数控制重试次数
                    match download_chunk(&client, &url, start, end, &temp_path).await {
                        Ok(bytes) => {
                            pb.inc(bytes.len() as u64);
                            let mut temp_files_lock = temp_files_clone.lock().unwrap();
                            (*temp_files_lock)[index] = Some(temp_path);
                            break;
                        }
                        Err(e) => {
                            retries += 1;
                            eprintln!(
                                "Error downloading chunk {}: {}. Retrying ({}/{})...",
                                i, e, retries, max_retries
                            );
                            if retries == max_retries {
                                eprintln!(
                                    "Failed to download chunk {} after {} retries",
                                    i, max_retries
                                );
                            }
                        }
                    }
                }
            }));
        }

        join_all(tasks).await;

        // 解锁 temp_files 并合并文件
        let temp_files_final = temp_files.lock().unwrap();
        let mut file = File::create(&file_path).await.expect("Create file failed");
        for (i, temp_path) in temp_files_final.iter().enumerate() {
            if let Some(path) = temp_path {
                let mut temp_file = File::open(path).await.expect("Open temp file failed");
                let mut buffer = Vec::new();
                temp_file
                    .read_to_end(&mut buffer)
                    .await
                    .expect("Read temp file failed");
                file.write_all(&buffer).await.expect("Write file failed");
            } else {
                eprintln!("Skipping chunk {} as it failed to download", i);
            }
        }

        temp_dir.close().expect("Remove temp dir failed");
        println!("Download complete!");
        println!("File saved at: {}", file_path.display());
    } else {
        println!("Server does not support range requests");
    }
}

async fn download_chunk(
    client: &reqwest::Client,
    url: &str,
    start: u64,
    end: u64,
    temp_path: &Path,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let response = client
        .get(url)
        .header(RANGE, format!("bytes={}-{}", start, end))
        .send()
        .await?;
    let bytes = response.bytes().await?;
    let mut file = File::create(temp_path).await?;
    file.write_all(&bytes).await?;
    Ok(bytes.to_vec())
}
