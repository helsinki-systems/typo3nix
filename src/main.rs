use futures::future::try_join_all;
use indexmap::IndexMap;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use ssri::{Algorithm, IntegrityOpts};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::{env, fs::File};

const API: &str = "https://extensions.typo3.org/api/v1";
const PER_PAGE: u32 = 50;

#[derive(Deserialize, Serialize)]
struct ExtensionCurrentVersion {
    description: String,
    number: String,
    typo3_versions: Vec<u32>,
}

#[derive(Deserialize, Serialize)]
struct ExtensionResponse {
    key: String,
    current_version: ExtensionCurrentVersion,
}

#[derive(Deserialize, Serialize)]
struct ExtensionsResponse {
    results: u32,
    page: u32,
    per_page: u32,
    extensions: Vec<ExtensionResponse>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ExtensionOutput {
    version: String,
    t3_versions: Vec<u32>,
    description: String,
    hash: String,
}

async fn request_page(
    client: &Client,
    pageno: u32,
    per_page: u32,
) -> Result<ExtensionsResponse, reqwest::Error> {
    client
        .get(format!("{}/extension", API))
        .query(&[("page", pageno), ("per_page", per_page)])
        .basic_auth(
            env::var("TYPO3NIX_USER").expect("TYPO3NIX_USER not set"),
            Some(env::var("TYPO3NIX_PASSWORD").expect("TYPO3NIX_PASSWORD not set")),
        )
        .send()
        .await?
        .json()
        .await
}

async fn calc_hash(client: &Client, url: &str) -> Result<String, reqwest::Error> {
    let mut integrity = IntegrityOpts::new().algorithm(Algorithm::Sha256);

    let mut res = client
        .get(url)
        .basic_auth(
            env::var("TYPO3NIX_USER").expect("TYPO3NIX_USER not set"),
            Some(env::var("TYPO3NIX_PASSWORD").expect("TYPO3NIX_PASSWORD not set")),
        )
        .send()
        .await?;

    while let Some(chunk) = res.chunk().await? {
        integrity.input(chunk);
    }

    Ok(format!("{}", integrity.result()))
}

async fn handle_extension(
    client: &Client,
    input: ExtensionResponse,
    out: Arc<Mutex<IndexMap<String, ExtensionOutput>>>,
    old_json: &IndexMap<String, ExtensionOutput>,
) -> Result<(), reqwest::Error> {
    let key = input.key;
    let url = format!("https://extensions.typo3.org/extension/download/{}/{}/zip", key, input.current_version.number);
    let hash = if let Some(old) = old_json.get(&key) {
        if old.version == input.current_version.number && !old.hash.is_empty() {
            old.hash.clone()
        } else {
            match calc_hash(client, &url).await {
                Ok(hash) => hash,
                Err(e) => {
                    println!("Unable to calculate hash of {}: {}", key, e);
                    return Err(e);
                }
            }
        }
    } else {
        match calc_hash(client, &url).await {
            Ok(hash) => hash,
            Err(e) => {
                println!("Unable to calculate hash of {}: {}", key, e);
                return Err(e);
            }
        }
    };

    out.lock().unwrap().insert(
        key,
        ExtensionOutput {
            version: input.current_version.number,
            t3_versions: input.current_version.typo3_versions,
            description: input.current_version.description.split('\n').next().unwrap().to_string(),
            hash,
        },
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), reqwest::Error> {
    let client = Client::new();
    let test_mode = env::var("TYPO3NIX_TEST_MODE").unwrap_or_else(|_| "0".to_string()) == "1";
    let per_page = if test_mode { 1 } else { PER_PAGE };

    // Load the old json
    let old_json: IndexMap<String, ExtensionOutput> = if Path::new("extensions.json").exists() {
        let file = File::open("extensions.json").expect("Failed to open extensions.json");
        serde_json::from_reader(file).expect("Failed to load extensions.json")
    } else {
        IndexMap::new()
    };

    // Ensure we quit better
    let quitting = Arc::new(AtomicBool::new(false));
    let quitting2 = Arc::clone(&quitting);
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.unwrap();
        println!("Quitting after this page");
        quitting2.store(true, Ordering::Relaxed);
    });

    // Iterate over pages
    let mut pages = 1;
    let mut page = 1;
    let new_json = Arc::new(Mutex::new(IndexMap::new()));
    let mut futures = Vec::new();
    loop {
        if page > pages || quitting.load(Ordering::Relaxed) {
            break;
        }
        println!("At page {}/{}", page, pages);
        let this_page = request_page(&client, page, per_page).await.expect("Failed to read search page");
        if pages == 1 {
            // Initialize pages
            pages = (this_page.results + per_page - 1) / per_page;
            new_json.lock().unwrap().reserve(pages as usize);
        }
        // Handle extensions
        for extension in this_page.extensions {
            futures.push(handle_extension(
                &client,
                extension,
                Arc::clone(&new_json),
                &old_json,
            ));
        }
        if test_mode {
            println!("Test mode - success");
            return Ok(());
        }
        page += 1;
    }
    println!("Waiting for remaining hash calulcations...");
    try_join_all(futures).await?;

    // Write JSON
    new_json.lock().unwrap().sort_keys();
    let file = File::create("extensions.json").expect("Failed to create/truncate extensions.json");
    serde_json::to_writer_pretty(file, &new_json).expect("Failed to serialize JSON");
    Ok(())
}
