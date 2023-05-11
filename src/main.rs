use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};
use atomic_counter::{AtomicCounter, RelaxedCounter};
use reqwest::{Proxy, StatusCode};
use reqwest::redirect::Policy;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::{mpsc};
use tokio::{signal, task};
use tokio::task::{JoinSet};
use tokio::time::sleep;
use rand::seq::SliceRandom;
use tokio_util::sync::CancellationToken;

// a-z, A-Z, 0-9
const CHARS: &[char; 62] = &['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];
const ID_LEN: usize = 7; // modern id length, AT already has a list of all working 5char ids

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 {
        println!("Usage: imgur_id <output> <concurrent> <proxies>");
        println!("\toutput: Path to the output file, will be appended to");
        println!("\tconcurrent: How many requests to queue per second max. (actual rate will be slightly lower)");
        println!("\tproxies: Proxy list file in the format of 'PROXY_HOST:PROXY_PORT:PROXY_USER:PROXY_PASSWORD' with one entry per line");
        println!("\t         So for example 'proxy.example.com:1234:username:password123'");
        println!("\t         For each entry, one worker will be spawned.");
        std::process::exit(1);
    }
    let mut proxies = vec![];
    // read proxies:
    match File::open(args[3].as_str()).await {
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
                        println!("Failed to read line from proxy file '{}': {}", args[4], e);
                        std::process::exit(1);
                    }
                }
            }
        }
        Err(e) => {
            println!("Failed to open file '{}': {}", args[4], e);
            std::process::exit(1);
        }
    }
    //
    let out_file = Arc::new(args[1].clone());
    let concurrent: usize = args[2].parse().unwrap();
    let (producer, consumer) = async_channel::bounded(concurrent);
    let producer = Arc::new(producer);
    let consumer = Arc::new(consumer);
    let stopped = CancellationToken::new();
    let mut tasks = JoinSet::<()>::new();
    {
        let producer = producer.clone();
        let stopped = stopped.clone();
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
                if dispatched == concurrent {
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
    let (done_tx, mut done_rx) = mpsc::channel(concurrent * 10);
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
        worker_counter += 1;
        let worker_i = worker_counter;
        tasks.spawn(async move {
            // slowly ramp up workers so we don't spam everything at once at the start
            let r = worker_i as f32 / concurrent as f32 * 10000.0;
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
                                            println!("Failed to request '{}', got status {}, retrying in 3min", url, status.as_u16());
                                            tokio::select! {
                                                _ = sleep(Duration::from_secs(180)) => {}
                                                _ = stopped.cancelled() => {
                                                    println!("Worker #{} is done.", worker_i);
                                                    return;
                                                }
                                            }
                                            continue;
                                        }
                                        tasks_failed.inc();
                                    }
                                    if worked % concurrent == 0 {
                                        let found = tasks_found.get();
                                        let failed = tasks_failed.get();
                                        let elapsed = start.elapsed();
                                        println!("Worked {}, found {}, failed {}, ~{:.1}% exist, {:.1} req/s", worked, found, failed, found as f32 / worked as f32 * 100.0, worked as f32 / elapsed.as_secs_f32());
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
        match OpenOptions::new()
            .write(true)
            .append(true)
            .create(true)
            .open(out_file.as_str()).await {
            Ok(mut filef) => {
                let mut file = BufWriter::new(&mut filef);
                loop {
                    if let Some(found) = done_rx.recv().await {
                        match file.write_all(format!("https://i.imgur.com/{}.jpg", found).as_bytes()).await {
                            Ok(_) => {}
                            Err(e) => {
                                println!("Failed to write result to results file '{}': {}", out_file, e);
                                return;
                            }
                        }
                        match file.write_all(b"\n").await {
                            Ok(_) => {}
                            Err(e) => {
                                println!("Failed to write result to results file '{}': {}", out_file, e);
                                return;
                            }
                        }
                    } else {
                        match file.flush().await {
                            Ok(_) => {}
                            Err(e) => {
                                println!("Failed to close results file after being done '{}': {}", out_file, e);
                                return;
                            }
                        }
                        match filef.shutdown().await {
                            Ok(_) => {}
                            Err(e) => {
                                println!("Failed to close results file after being done '{}': {}", out_file, e);
                                return;
                            }
                        }
                        println!("Finished writing results");
                        return; // channel closed
                    }
                }
            }
            Err(e) => {
                println!("Failed to open results file '{}': {}", out_file, e);
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
    println!("Worked {:07}, found {:07}, failed {:07}, ~{:.1}% exist, {:.1} req/s", worked, found, failed, found as f32 / worked as f32 * 100.0, worked as f32 / elapsed.as_secs_f32());
    println!("All done.");
}