use std::fs::File;
use std::io::{BufReader, BufRead};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use std::sync::{Arc, mpsc, Mutex};
use std::thread;
use serde_json::json;
use std::fs::OpenOptions;
use std::io::Write;
use std::env;

type Job = Box<dyn FnOnce() + Send + 'static>;

enum Message {
    Job(Job),
    Shutdown,
}

struct WebsiteStatus {
    url: String,
    status: Result<u16, String>,
    response_time: u128,
    timestamp: u64,
}

struct ThreadPool {
    workers: Vec<Worker>,
    sender: mpsc::Sender<Message>,
}


struct Worker {
    id: usize,
    thread: Option::<thread::JoinHandle<()>>,
}


impl Worker {
    fn new(id: usize, receiver: Arc<Mutex<mpsc::Receiver<Message>>>) -> Self {
        let thread = thread::spawn(move || loop {
            let message = receiver.lock().unwrap().recv();
            match message {
                Ok(Message::Job(job)) => {
                    println!("Worker {} got a job; executing.", id);
                    job();
                }
                Ok(Message::Shutdown) => {
                    println!("Worker {} received shutdown signal.", id);
                    break;
                }
                Err(_) => {
                    println!("Worker {} shutting down due to channel close.", id);
                    break;
                }
            }
        });
        Self{
            id,
            thread: Some(thread),
        }
    }
}

impl WebsiteStatus {
    fn save_to_file(&self) {
        let json_data = json!({
            "url": self.url,
            "status": match &self.status{
                Ok(code) => code.to_string(),
                Err(err) => err.clone(),
            },
            "response_time": self.response_time,
            "timestamp": self.timestamp
        });

        let file_result = OpenOptions::new()
            .create(true)
            .append(true)
            .open("website_status_checked.json");
    
        if let Ok(mut file) = file_result {
            serde_json::to_writer_pretty(&mut file, &json_data).expect("Failed to write to file");
            writeln!(file).expect("Failed to write newline");
            println!("Website has been checked and saved to file!");
        } else {
            println!("Failed to write to file");
        }
    }
    
}

impl ThreadPool {

    pub fn new(size: usize) -> Self{
        let (sender, receiver) = mpsc::channel();
        let receiver = Arc::new(Mutex::new(receiver));

        let workers = (0..size)
            .map(|id| Worker::new(id, Arc::clone(&receiver)))
            .collect();

        Self { workers, sender }
    }

    pub fn execute<F>(&self, job: F)
    where 
        F: FnOnce() + Send + 'static,
    {
        self.sender.send(Message::Job(Box::new(job))).unwrap();
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self){

        println!("Shutting down all workers...");

        for _ in &self.workers {
            self.sender.send(Message::Shutdown).unwrap();
        }
        for worker in &mut self.workers {
            println!("Shutting down worker {}", worker.id);

            if let Some(thread) = worker.thread.take() {
                thread.join().unwrap();
            }
        }
    }
}

fn read_file() -> Vec<String> {
    let file = File::open("Websites.txt").expect("Failed to open file");
    let reader = BufReader::new(file);
    let mut websites = Vec::new();

    for line in reader.lines() {
        match line{
            Ok(content) => websites.push(content),
            Err(e) => eprintln!("Error reading line: {}", e),
        }
    }
    websites
}


fn check_website(url: &str, timeout: u64) -> WebsiteStatus {
    let start_time = Instant::now();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let timeout_duration = std::time::Duration::from_secs(get_timeout());
    let response = ureq::get(url)
        .timeout(timeout_duration)
        .call();

    let status = if response.is_ok(){
        Ok(response.unwrap().status())
    } else{
        Err(response.unwrap_err().to_string())
    };

    let response_time = start_time.elapsed().as_millis();

    WebsiteStatus{
        url: url.to_string(),
        status,
        response_time,
        timestamp,
    }
}

fn get_timeout() -> u64 {
    
    let args: Vec<String> = env::args().collect();
    if let Some(pos) = args.iter().position(|x| x == "--timeout") {
        if let Some(value) = args.get(pos + 1){
            return value.parse().unwrap_or_else(|_| {
                eprintln!("Invalid timeout value. Falling back to default of 5 seconds.");
                5        
            });
        }
    }

    5
}

fn get_worker_thread_count() -> usize {

    let args: Vec<String> = env::args().collect();
    if let Some(pos) = args.iter().position(|x| x == "--threads") {
        if let Some(value) = args.get(pos + 1) {
            return value.parse().unwrap_or_else(|_|{
                eprintln!("Invalid thread count value. Falling back to default of 4 threads");
                4
            });
        }
    }
    4
}


fn check_website_with_retries(url: &str, retries: usize, timeout: u64) -> WebsiteStatus {

    for attempt in 0..=retries {
        let result = check_website(url, timeout);
        if result.status.is_ok() {
            return result;
        }
        println!(
            "Attempt {}/{} for {} failed. Retrying...",
            attempt + 1,
            retries,
            url
        );
    }

    check_website(url, timeout)
}

fn get_max_retries() -> usize {
    env::args()
        .find(|arg| arg.starts_with("--retries"))
        .and_then(|arg| arg.split("=").nth(1).map(|val| val.to_string()))
        .and_then(|val| val.parse().ok())
        .unwrap_or(3)
}

fn get_interval() -> usize {
    env::args()
        .find(|arg| arg.starts_with("--interval"))
        .and_then(|arg| arg.split("=").nth(1).map(|val| val.to_owned()))
        .and_then(|val| val.parse().ok())
        .unwrap_or(30)
}

fn get_cycles() -> Option<usize> {
    env::args()
        .find(|arg| arg.starts_with("--cycle"))
        .and_then(|arg| arg.split("=").nth(1).map(|val| val.to_owned()))
        .and_then(|val| val.parse().ok())
}

fn main() {
    let list_of_websites = Arc::new(read_file());

    let thread_count = get_worker_thread_count();
    println!("Using {} worker threads", thread_count);

    let timeout = get_timeout();
    let retries = get_max_retries();
    let interval = get_interval();
    let cycles = get_cycles();

    let pool: ThreadPool = ThreadPool::new(thread_count);


    match cycles{
        Some(cycles) => {
            for _ in 0..cycles{
                for url in list_of_websites.iter().cloned(){
                    let url_clone = url.clone();
                    pool.execute(move || {
                        let website_status = check_website_with_retries(&url_clone, retries, timeout);
                        website_status.save_to_file();
                    });
                }
                thread::sleep(std::time::Duration::from_secs(interval as u64));
            }
        }
        None => {
            loop{

                for url in list_of_websites.iter().cloned(){
                    let url_clone = url.clone();
                    pool.execute(move || {
                        let website_status = check_website_with_retries(&url_clone, retries, timeout);
                        website_status.save_to_file();
                    });
                }
        
                thread::sleep(std::time::Duration::from_secs(interval as u64));
            }
        }
    }

}
