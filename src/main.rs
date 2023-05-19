use std::process::exit;
use std::sync::Arc;
use std::time::{Duration, Instant};
use atomic_counter::{AtomicCounter, RelaxedCounter};
use clap::Parser;
use reqwest::{Client, ClientBuilder, Proxy, StatusCode};
use reqwest::redirect::Policy;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::{mpsc};
use tokio::{signal, task};
use tokio::task::{JoinSet};
use tokio::time::sleep;
use rand::seq::SliceRandom;
use tokio_util::sync::CancellationToken;
use serde::Serialize;
use clap::crate_version;

// a-z, A-Z, 0-9
const CHARS: &[char; 62] = &['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];
const ID_LEN: usize = 7; // modern id length, AT already has a list of all working 5char ids

const NO_PROXY_CONC_LIMIT: usize = 10;

/// Simple program to greet a person
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Where to save found results
    #[arg(short, long)]
    pub results_file: Option<String>,
    /// This specifies an optional list of http proxies to use
    /// Proxy list file has the format of 'PROXY_HOST:PROXY_PORT:PROXY_USER:PROXY_PASSWORD' with one entry per line
    /// So for example 'proxy.example.com:1234:username:password123'
    /// For each entry, one worker will be spawned.
    #[arg(short, long, verbatim_doc_comment)]
    pub proxy_file: Option<String>,

    /// If used, results will not be reported automatically
    #[arg(short, long, default_value_t = false)]
    pub offline: bool,

    /// Url to an alternative result tracker, results are POST'ed to the url with a json body
    /// in the format of {"images_found": ["AsDfgHi", "7654321", "1234567", ...]}
    /// Defaults to nicolas17's tracker
    #[arg(long, verbatim_doc_comment)]
    pub online_tracker_url: Option<String>,

    ///  How many requests to queue per second (actual rate will be slightly lower)
    #[arg(short, long, default_value_t = 3)]
    pub concurrent: usize,
    /// Bypass concurrency sanity check
    #[arg(short, long, default_value_t = false)]
    pub concurrent_unsafe: bool,
}

struct ResultsFile {
    writer: BufWriter<File>,
}

impl ResultsFile {
    pub async fn open(path: &str) -> ResultsFile {
        match OpenOptions::new()
            .write(true)
            .append(true)
            .create(true)
            .open(path).await {
            Ok(filef) => {
                return ResultsFile {
                    writer: BufWriter::new(filef)
                };
            }
            Err(e) => {
                println!("Failed to open results file '{}': {}", path, e);
                exit(1)
            }
        }
    }

    pub async fn write(&mut self, found: &str) -> bool {
        match self.writer.write_all(format!("https://i.imgur.com/{}.jpg", found).as_bytes()).await {
            Ok(_) => {}
            Err(e) => {
                println!("Failed to write result to results file: {}", e);
                return false;
            }
        }
        match self.writer.write_all(b"\n").await {
            Ok(_) => {}
            Err(e) => {
                println!("Failed to write result to results file: {}", e);
                return false;
            }
        }
        return true;
    }

    pub async fn close(mut self) {
        match self.writer.flush().await {
            Ok(_) => {}
            Err(e) => {
                println!("Failed to close results file after being done: {}", e);
                return;
            }
        }
        match self.writer.into_inner().shutdown().await {
            Ok(_) => {}
            Err(e) => {
                println!("Failed to close results file after being done: {}", e);
                return;
            }
        }
    }
}
#[derive(Serialize, Debug)]
struct ResultsTrackerBuffer {
    pub images_found: Vec<String>,
}
struct ResultsTracker {
    client: Client,
    url: String,
    buffer: ResultsTrackerBuffer,
    last_send: Instant,
}

const TRACKER_SEND_INTERVAL: Duration = Duration::from_secs(5);
const TRACKER_SEND_LIMIT: usize = 100;

impl ResultsTracker {
    pub fn new(url: Option<String>) -> ResultsTracker {
        ResultsTracker {
            client: ClientBuilder::new().user_agent(format!("imgur_id7/{}", crate_version!())).build().unwrap(),
            url: url.unwrap_or(String::from("https://data.nicolas17.xyz/imgur-bruteforce/report")),
            buffer: ResultsTrackerBuffer{images_found: vec![]},
            last_send: Instant::now()
        }
    }

    pub async fn report(&mut self, id: String) {
        self.buffer.images_found.push(id);
        if self.last_send.elapsed() > TRACKER_SEND_INTERVAL ||
            self.buffer.images_found.len() > TRACKER_SEND_LIMIT {
            self.send().await;
        }
    }

    async fn send(&mut self) {
        loop {
            match self.client.post(&self.url).json(&self.buffer).send().await {
                Ok(res) => {
                    if !res.status().is_success(){
                        println!("Failed to submit results to the tracker, got non-2oo status {}. Retrying in 2s. Response: {}\n", res.status().as_u16(), res.text().await.unwrap_or(String::from("n/a")));
                    } else {
                        println!("Reported {} to the tracker. Thanks!", self.buffer.images_found.len());
                        break;
                    }
                }
                Err(e) => {
                    println!("Failed to submit results to the tracker: {}. Retrying in 2s", e);
                }
            }
            sleep(Duration::from_secs(2)).await;
        }
        self.last_send = Instant::now();
        self.buffer.images_found.clear();
    }

    pub async fn close(mut self) {
        if self.buffer.images_found.len() > 0 {
            self.send().await;
        }
    }
}
#[tokio::main]
async fn main() {
    let args = Arc::new(Args::parse());

    let mut proxies = vec![];
    if let Some(proxy_list) = &args.proxy_file {
        match File::open(proxy_list.as_str()).await {
            Ok(file) => {
                let reader = BufReader::new(file);

                let mut lines = reader.lines();

                loop {
                    match lines.next_line().await {
                        Ok(l) => {
                            if let Some(line) = l {
                                if line.is_empty() {
                                    continue;
                                }
                                let mut splits: Vec<&str> = line.as_str().split(":").collect();
                                if splits.len() < 2 {
                                    println!("Proxy line \"{}\" was malformed", line);
                                }
                                while splits.len() < 4 {
                                    splits.push("");
                                }
                                let purl = format!("http://{}:{}@{}:{}/", splits[2], splits[3], splits[0], splits[1]);
                                match Proxy::all(purl.as_str()) {
                                    Ok(proxy) => {
                                        proxies.push(proxy)
                                    }
                                    Err(e) => {
                                        println!("Bad proxy line '{}' -> '{}': {}", line, purl, e);
                                        std::process::exit(1);
                                    }
                                }
                            } else {
                                break;
                            }
                        }
                        Err(e) => {
                            println!("Failed to read line from proxy file '{}': {}", proxy_list, e);
                            std::process::exit(1);
                        }
                    }
                }
            }
            Err(e) => {
                println!("Failed to open file '{}': {}", proxy_list, e);
                std::process::exit(1);
            }
        }
    } else {
        if args.concurrent > NO_PROXY_CONC_LIMIT && !args.concurrent_unsafe {
            println!("Concurrency seems to be set too high for a single ip. (max. {NO_PROXY_CONC_LIMIT}), refusing to start.\nIf you're really sure you want this, prefix the number with ! and I'll do it.");
            exit(1);
        }
        for _ in 0..args.concurrent {
            proxies.push(Proxy::custom(|_| -> Option<&'static str> { None }));
        }
    }
    //
    let (producer, consumer) = async_channel::bounded(args.concurrent + 10);
    let producer = Arc::new(producer);
    let consumer = Arc::new(consumer);
    let stopped = CancellationToken::new();
    let mut tasks = JoinSet::<()>::new();
    {
        let producer = producer.clone();
        let stopped = stopped.clone();
        let args = args.clone();
        tasks.spawn(async move {
            let mut dispatched = 0usize;
            loop {
                let mut line = String::new();
                line.reserve(ID_LEN);
                {
                    let mut rng = rand::thread_rng();
                    for _ in 0..ID_LEN {
                        line.push(*CHARS.choose(&mut rng).unwrap());
                    }
                    drop(rng);
                }
                match producer.send(line).await {
                    Ok(_) => {}
                    Err(e) => {
                        println!("Failed to send task {}", e);
                        producer.close();
                        return;
                    }
                };
                dispatched += 1;
                if dispatched == args.concurrent {
                    dispatched = 0;
                    if stopped.is_cancelled() {
                        break;
                    }
                    // stupid rate limiting
                    // this could be better, but it'll do *shrug*
                    sleep(Duration::from_secs(1)).await
                }
            }

            println!("Producer is done.");
            producer.close();
        });
    }
    let (done_tx, mut done_rx) = mpsc::channel(args.concurrent * 10);
    let done_tx = Arc::new(done_tx);
    let tasks_worked = Arc::new(RelaxedCounter::new(0));
    let tasks_found = Arc::new(RelaxedCounter::new(0));
    let tasks_failed = Arc::new(RelaxedCounter::new(0));
    let start = Instant::now();
    let mut worker_counter = 0;
    for proxy in proxies {
        let tasks_worked = tasks_worked.clone();
        let tasks_found = tasks_found.clone();
        let tasks_failed = tasks_failed.clone();
        let consumer = consumer.clone();
        let done_tx = done_tx.clone();
        let stopped = stopped.clone();
        let args = args.clone();
        worker_counter += 1;
        let worker_i = worker_counter;
        tasks.spawn(async move {
            // slowly ramp up workers so we don't spam everything at once at the start
            let r = worker_i as f32 / args.concurrent as f32 * 10000.0;
            sleep(std::time::Duration::from_millis(r as u64)).await;
            let client = reqwest::Client::builder()
                .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:109.0) Gecko/20100101 Firefox/112.0")
                .proxy(proxy.clone())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(10))
                .redirect(Policy::none())
                .build();
            if client.is_err() {
                println!("Failed to build http client: {}", client.unwrap_err());
                return;
            }
            let client = client.unwrap();
            loop {
                match consumer.recv().await {
                    Ok(task) => {
                        let url = format!("https://i.imgur.com/{}.jpg", task);
                        // 10 attempts to fetch the id due to network errors
                        // it isnt super critical we complete this request, we should
                        const ATTEMPTS: usize = 10;
                        for i in 0..ATTEMPTS {
                            match client.head(url.as_str())
                                .send()
                                .await {
                                Ok(res) => {
                                    //println!("{}: {}", url, res.status());
                                    let worked = tasks_worked.inc() + 1;
                                    if res.status().is_success() {
                                        tasks_found.inc();
                                        match done_tx.send(task).await {
                                            Ok(_) => {}
                                            Err(e) => {
                                                println!("Failed to send result {}", e);
                                                return;
                                            }
                                        }
                                    } else {
                                        let status = res.status();
                                        if status == StatusCode::TOO_MANY_REQUESTS ||
                                            status.is_server_error() {
                                            let dur;
                                            if status == StatusCode::TOO_MANY_REQUESTS {
                                                dur = 90;
                                            } else {
                                                dur = 3;
                                            }
                                            println!("Failed to request '{}', got status {}, retrying in {}min", url, status.as_u16(), dur);
                                            tokio::select! {
                                                _ = sleep(Duration::from_secs(dur * 60)) => {}
                                                _ = stopped.cancelled() => {
                                                    println!("Worker #{} is done.", worker_i);
                                                    return;
                                                }
                                            }
                                            continue;
                                        }
                                        tasks_failed.inc();
                                    }
                                    if worked % args.concurrent == 0 {
                                        let found = tasks_found.get();
                                        let failed = tasks_failed.get();
                                        let elapsed = start.elapsed();
                                        println!("Worked {}, found {}, failed {}, ~{:.2}% exist, {:.1} req/s", worked, found, failed, found as f32 / worked as f32 * 100.0, worked as f32 / elapsed.as_secs_f32());
                                    }
                                    break;
                                }
                                Err(e) => {
                                    let dur;
                                    if i == ATTEMPTS - 1 {
                                        // reduce log spam by pausing bad workers for a while
                                        println!("Reached max. attempts for request '{}', pausing worker for 5min: {}", url, e);
                                        dur = Duration::from_secs(60 * 5);
                                    } else {
                                        println!("Failed to request '{}', retrying in 500ms: {}", url, e);
                                        dur = Duration::from_millis(500);
                                    }
                                    tokio::select! {
                                                _ = sleep(dur) => {}
                                                _ = stopped.cancelled() => {
                                                    println!("Worker #{} is done.", worker_i);
                                                    return;
                                                }
                                            }
                                }
                            }
                        }
                    }
                    Err(_) => {
                        // closed channel, done!
                        println!("Worker #{} is done.", worker_i);
                        return;
                    }
                }
            }
        });
    }
    drop(done_tx);
    let result_task = task::spawn(async move {
        let mut result_file = None;
        if let Some(out_file) = &args.results_file {
            result_file = Some(ResultsFile::open(out_file.as_str()).await);
        }
        let mut result_tracker = None;
        if !args.offline {
            result_tracker = Some(ResultsTracker::new(args.online_tracker_url.clone()));
        }

        if result_tracker.is_none() && result_file.is_none() {
            println!("No result output mechanism selected.\nEither writing to file or sending to tracker need to be enabled!");
            exit(1);
        }
        loop {
            if let Some(found) = done_rx.recv().await {
                if let Some(file) = result_file.as_mut() {
                    if !file.write(found.as_str()).await {
                        return;
                    }
                }
                if let Some(tracker) = result_tracker.as_mut() {
                    tracker.report(found).await;
                }
            } else {
                if let Some(file) = result_file {
                    file.close().await;
                }
                if let Some(tracker) = result_tracker {
                    tracker.close().await;
                }
                println!("Finished writing results");
                return; // channel closed
            }
        }
    });
    println!("Running, press ctrl+c to cancel");
    match signal::ctrl_c().await {
        Ok(()) => {
            println!("Got ctrl+c, shutting down gracefully");
            stopped.cancel();
        }
        Err(err) => {
            eprintln!("Unable to listen for shutdown signal: {}", err);
            // we also shut down in case of error
        }
    }
    while let Some(res) = tasks.join_next().await {
        match res {
            Ok(_) => {}
            Err(e) => {
                println!("Failed to join worker task: {}", e)
            }
        }
    }
    println!("Producer/Workers finished. Waiting for result writer now..");
    match result_task.await {
        Ok(_) => {}
        Err(e) => {
            println!("Failed to join result task: {}", e)
        }
    }
    let elapsed = start.elapsed();
    let worked = tasks_worked.get();
    let found = tasks_found.get();
    let failed = tasks_failed.get();
    println!("Worked {:07}, found {:07}, failed {:07}, ~{:.2}% exist, {:.1} req/s", worked, found, failed, found as f32 / worked as f32 * 100.0, worked as f32 / elapsed.as_secs_f32());
    println!("All done.");
}