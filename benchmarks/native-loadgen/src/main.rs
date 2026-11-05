//! Bounded open-loop HTTP/1.1 load generator, with persistent pooled connections.
use serde_json::{Value, json};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    sync::{Mutex, Semaphore},
    task::JoinSet,
    time::{Instant, sleep_until},
};
type Connection = BufReader<TcpStream>;
type Pool = Arc<Mutex<Vec<Connection>>>;
const BINS: usize = 80001;
#[derive(Clone)]
struct Histogram {
    bins: Vec<u64>,
    count: u64,
}
impl Histogram {
    fn new() -> Self {
        Self {
            bins: vec![0; BINS],
            count: 0,
        }
    }
    fn add(&mut self, micros: u64) {
        let bin = if micros <= 100_000 {
            micros.div_ceil(10)
        } else if micros <= 1_000_000 {
            10000 + (micros - 100000).div_ceil(100)
        } else {
            19000 + (micros - 1000000).div_ceil(1000)
        };
        self.bins[(bin as usize).min(BINS - 1)] += 1;
        self.count += 1;
    }
    fn percentile(&self, p: u64) -> Option<f64> {
        if self.count == 0 {
            return None;
        }
        let rank = (self.count * p).div_ceil(100);
        let mut count = 0;
        for (i, n) in self.bins.iter().enumerate() {
            count += n;
            if count >= rank {
                return Some(if i <= 10000 {
                    i as f64 / 100.0
                } else if i <= 19000 {
                    100.0 + (i - 10000) as f64 / 10.0
                } else {
                    1000.0 + (i - 19000) as f64
                });
            }
        }
        None
    }
    fn json(&self) -> Value {
        json!({"p50_ms":self.percentile(50),"p95_ms":self.percentile(95),"p99_ms":self.percentile(99)})
    }
}
async fn exchange(pool: Pool, identity: String) -> io::Result<u16> {
    exchange_at(pool, identity, "server:8080").await
}
async fn exchange_at(pool: Pool, identity: String, address: &str) -> io::Result<u16> {
    let pooled = pool.lock().await.pop();
    let mut connection = if let Some(c) = pooled {
        c
    } else {
        let stream = TcpStream::connect(address).await?;
        stream.set_nodelay(true)?;
        BufReader::new(stream)
    };
    connection.write_all(format!("GET / HTTP/1.1\r\nHost: server\r\nx-bench-client: {identity}\r\nConnection: keep-alive\r\n\r\n").as_bytes()).await?;
    let mut line = String::new();
    connection.read_line(&mut line).await?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| io::Error::other("invalid HTTP status"))?;
    let (mut length, mut chunked, mut close) = (None, false, false);
    let mut headers = line.len();
    loop {
        line.clear();
        if connection.read_line(&mut line).await? == 0 {
            return Err(io::Error::other("truncated HTTP headers"));
        }
        headers += line.len();
        if headers > 65536 {
            return Err(io::Error::other("oversized HTTP headers"));
        }
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            match key.to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse::<usize>().ok(),
                "transfer-encoding" => chunked = value.to_ascii_lowercase().contains("chunked"),
                "connection" => close = value.to_ascii_lowercase().contains("close"),
                _ => {}
            }
        }
    }
    if chunked {
        let mut total = 0;
        loop {
            line.clear();
            connection.read_line(&mut line).await?;
            let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)
                .map_err(io::Error::other)?;
            if size == 0 {
                loop {
                    line.clear();
                    if connection.read_line(&mut line).await? == 0 || line == "\r\n" {
                        break;
                    }
                }
                break;
            }
            total += size;
            if total > 65536 {
                return Err(io::Error::other("oversized HTTP body"));
            }
            let mut body = vec![0; size + 2];
            connection.read_exact(&mut body).await?;
        }
    } else if let Some(size) = length {
        if size > 65536 {
            return Err(io::Error::other("oversized HTTP body"));
        }
        let mut body = vec![0; size];
        connection.read_exact(&mut body).await?;
    } else if !matches!(status, 204 | 304) {
        return Err(io::Error::other("HTTP response requires explicit framing"));
    }
    if !close {
        pool.lock().await.push(connection);
    }
    Ok(status)
}
struct Observation {
    client: usize,
    status: u16,
    latency: u64,
    lag: u64,
    end: Instant,
}
struct Results {
    latency: Histogram,
    scheduled_latency: Histogram,
    lag: Histogram,
    statuses: [u64; 5],
    status_latency: [Histogram; 3],
    attempts: Vec<usize>,
    admitted: Vec<usize>,
    count: u64,
    latency_sum: u128,
    completed: u64,
}
impl Results {
    fn new(clients: usize) -> Self {
        Self {
            latency: Histogram::new(),
            scheduled_latency: Histogram::new(),
            lag: Histogram::new(),
            statuses: [0; 5],
            status_latency: std::array::from_fn(|_| Histogram::new()),
            attempts: vec![0; clients],
            admitted: vec![0; clients],
            count: 0,
            latency_sum: 0,
            completed: 0,
        }
    }
    fn add(&mut self, r: Observation, deadline: Instant) {
        let index = match r.status {
            200 => 0,
            429 => 1,
            503 => 2,
            0 => 4,
            _ => 3,
        };
        self.statuses[index] += 1;
        self.latency.add(r.latency);
        self.lag.add(r.lag);
        self.scheduled_latency.add(r.latency + r.lag);
        if index < 3 {
            self.status_latency[index].add(r.latency);
        }
        if matches!(r.status, 200 | 429) {
            self.attempts[r.client] += 1;
        }
        if r.status == 200 {
            self.admitted[r.client] += 1;
        }
        self.count += 1;
        self.latency_sum += r.latency as u128;
        self.completed += u64::from(r.end <= deadline);
    }
}
async fn phase(
    seconds: u64,
    rate: u64,
    concurrency: usize,
    clients: usize,
    prefix: &str,
    limit: usize,
) -> Value {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(seconds);
    let pool: Pool = Arc::new(Mutex::new(Vec::new()));
    let permits = Arc::new(Semaphore::new(concurrency));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut tasks = JoinSet::new();
    let mut results = Results::new(clients);
    let (mut late, mut capacity) = (0, 0);
    let scheduled = seconds * rate;
    for i in 0..scheduled {
        let due = start + Duration::from_secs_f64(i as f64 / rate as f64);
        sleep_until(due).await;
        while let Some(result) = tasks.try_join_next() {
            results.add(result.expect("load worker panicked"), deadline);
        }
        if Instant::now().saturating_duration_since(due) > Duration::from_millis(100) {
            late += 1;
            continue;
        }
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            capacity += 1;
            continue;
        };
        let (pool, active, peak) = (pool.clone(), active.clone(), peak.clone());
        let client = i as usize % clients;
        let identity = format!("{prefix}-{client}");
        tasks.spawn(async move {
            let _permit = permit;
            let begin = Instant::now();
            let current = active.fetch_add(1, Ordering::Relaxed) + 1;
            peak.fetch_max(current, Ordering::Relaxed);
            let status = tokio::time::timeout(Duration::from_secs(3), exchange(pool, identity))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(0);
            let end = Instant::now();
            active.fetch_sub(1, Ordering::Relaxed);
            Observation {
                client,
                status,
                latency: (end - begin).as_micros() as u64,
                lag: begin.saturating_duration_since(due).as_micros() as u64,
                end,
            }
        });
    }
    sleep_until(deadline).await;
    while let Some(result) = tasks.join_next().await {
        results.add(result.expect("load worker panicked"), deadline);
    }
    let elapsed = (Instant::now() - start).as_secs_f64().max(seconds as f64);
    let divisor = elapsed.max(0.000001);
    let mut r = json!({"schema_version":2,"generator":"rust-open-loop","histogram_precision":"upper bounds: 0.01ms to 100ms; 0.1ms to 1s; 1ms thereafter",
        "target_rps":rate,"offered_rps":if seconds>0 {results.count as f64/seconds as f64} else {0.0},"achieved_rps":results.count as f64/divisor,
        "elapsed_seconds":elapsed,"measurement_seconds":seconds,"drain_seconds":(elapsed-seconds as f64).max(0.0),"scheduled":scheduled,"sample_count":results.count,
        "completed_in_window":results.completed,"completion_rps_in_window":if seconds>0 {results.completed as f64/seconds as f64} else {0.0},
        "dropped":late+capacity,"dropped_late":late,"dropped_capacity":capacity,"loadgen_saturated":late+capacity>0,
        "concurrency_limit":concurrency,"peak_in_flight":peak.load(Ordering::Relaxed),"mean_in_flight":results.latency_sum as f64/1e6/divisor,
        "http_200":results.statuses[0],"http_429":results.statuses[1],"http_503":results.statuses[2],"http_other":results.statuses[3],"errors":results.statuses[4],
        "max_admissions_per_client":results.admitted.iter().max().unwrap_or(&0),"expected_admissions":results.attempts.iter().map(|n|(*n).min(limit)).sum::<usize>(),
        "clients_observed":results.attempts.iter().filter(|n|**n>0).count(),"dispatch_lag":results.lag.json(),"scheduled_latency":results.scheduled_latency.json(),
        "status_latency":{"200":results.status_latency[0].json(),"429":results.status_latency[1].json(),"503":results.status_latency[2].json()}});
    for key in ["p50_ms", "p95_ms", "p99_ms"] {
        r[key] = results.latency.json()[key].clone();
    }
    r
}
fn number(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
fn cgroup() -> Value {
    let mut map = serde_json::Map::new();
    for name in [
        "cpu.stat",
        "memory.current",
        "memory.peak",
        "memory.events",
        "memory.stat",
    ] {
        map.insert(
            name.into(),
            std::fs::read_to_string(format!("/sys/fs/cgroup/{name}"))
                .unwrap_or_default()
                .into(),
        );
    }
    Value::Object(map)
}
#[tokio::main(worker_threads = 2)]
async fn main() {
    let (rate, concurrency, clients, limit) = (
        number("RATE", 1000),
        number("CONCURRENCY", 128) as usize,
        number("CLIENTS", 1) as usize,
        number("LIMIT", 100) as usize,
    );
    assert!(
        (1..=100000).contains(&rate)
            && (1..=512).contains(&concurrency)
            && (1..=10000).contains(&clients)
    );
    let pool = Arc::new(Mutex::new(Vec::new()));
    let mut ready = false;
    for _ in 0..100 {
        if matches!(
            tokio::time::timeout(
                Duration::from_secs(1),
                exchange(pool.clone(), "readiness".into())
            )
            .await,
            Ok(Ok(200 | 429))
        ) {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ready, "server readiness failed");
    drop(pool);
    let (warmup, duration) = (number("WARMUP", 5), number("DURATION", 20));
    assert!(warmup <= 15 && (1..=60).contains(&duration));
    phase(warmup, rate, concurrency, clients, "warmup", limit).await;
    let before = cgroup();
    let mut result = phase(duration, rate, concurrency, clients, "measure", limit).await;
    result["loadgen_cgroup_before"] = before;
    result["loadgen_cgroup"] = cgroup();
    println!("{result}");
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn histogram_bounds_are_conservative_and_bounded() {
        let mut h = Histogram::new();
        assert_eq!(h.percentile(99), None);
        for micros in [1, 100001, 1000001] {
            h.add(micros);
        }
        assert_eq!(h.percentile(50), Some(100.1));
        assert_eq!(h.percentile(99), Some(1001.0));
        assert_eq!(h.bins.len(), BINS);
    }
    #[tokio::test]
    async fn zero_warmup_has_zero_counts() {
        let r = phase(0, 100, 32, 1, "warmup", 20).await;
        assert_eq!(r["sample_count"], 0);
        assert_eq!(r["achieved_rps"], 0.0);
    }
    #[tokio::test]
    async fn client_reuses_framed_connection_and_reads_chunked_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            for reply in [
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
                "HTTP/1.1 429 Too Many Requests\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nno\r\n0\r\n\r\n",
            ] {
                let mut line = String::new();
                loop {
                    line.clear();
                    stream.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                }
                stream.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        let pool = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(
            exchange_at(pool.clone(), "test".into(), &address)
                .await
                .unwrap(),
            200
        );
        assert_eq!(
            exchange_at(pool.clone(), "test".into(), &address)
                .await
                .unwrap(),
            429
        );
        task.await.unwrap();
    }
}
