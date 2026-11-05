//! Bounded open-loop HTTP/1.1 load generator, with persistent pooled connections.
use serde_json::{Value, json};
use std::{
    io::{self, Write},
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
type Pool = Arc<ConnectionPool>;
const BINS: usize = 80001;
const QUEUED_TASK_LIMIT: usize = 16;
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
// Idle connections are recycled before a request is written; requests are never replayed.
struct IdleConnection {
    connection: Connection,
    returned: Instant,
}
#[derive(Default)]
struct PoolCounters {
    created: AtomicUsize,
    reused: AtomicUsize,
    expired: AtomicUsize,
    closed: AtomicUsize,
}
struct ConnectionPool {
    idle: Mutex<std::collections::VecDeque<IdleConnection>>,
    counters: PoolCounters,
    idle_timeout: Duration,
}
fn new_pool() -> Pool {
    Arc::new(ConnectionPool {
        idle: Mutex::new(std::collections::VecDeque::new()),
        counters: PoolCounters::default(),
        idle_timeout: Duration::from_secs(4),
    })
}
impl ConnectionPool {
    fn json(&self) -> Value {
        json!({"created":self.counters.created.load(Ordering::Relaxed),"reused":self.counters.reused.load(Ordering::Relaxed),"idle_expired":self.counters.expired.load(Ordering::Relaxed),"idle_unusable":self.counters.closed.load(Ordering::Relaxed),"idle_timeout_ms":self.idle_timeout.as_millis(),"automatic_request_retries":0})
    }
}
#[derive(Debug)]
struct ClientError {
    stage: &'static str,
    kind: String,
    reused: bool,
}
impl ClientError {
    fn io(stage: &'static str, error: io::Error) -> Self {
        Self {
            stage,
            kind: format!("{:?}", error.kind()),
            reused: false,
        }
    }
    fn protocol(stage: &'static str, kind: &str) -> Self {
        Self {
            stage,
            kind: kind.into(),
            reused: false,
        }
    }
    fn key(&self) -> String {
        format!("{}:{}", self.stage, self.kind)
    }
}
async fn exchange(pool: Pool, identity: String) -> Result<u16, ClientError> {
    exchange_at(pool, identity, "server:8080").await
}
async fn exchange_at(pool: Pool, identity: String, address: &str) -> Result<u16, ClientError> {
    let mut pooled = None;
    loop {
        let Some(idle) = pool.idle.lock().await.pop_front() else {
            break;
        };
        if idle.returned.elapsed() >= pool.idle_timeout {
            pool.counters.expired.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let mut probe = [0u8; 1];
        match idle.connection.get_ref().try_read(&mut probe) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                pooled = Some(idle.connection);
                break;
            }
            _ => {
                pool.counters.closed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    let reused = pooled.is_some();
    let mut connection = if let Some(connection) = pooled {
        pool.counters.reused.fetch_add(1, Ordering::Relaxed);
        connection
    } else {
        let stream = TcpStream::connect(address)
            .await
            .map_err(|e| ClientError::io("connect", e))?;
        stream
            .set_nodelay(true)
            .map_err(|e| ClientError::io("connect", e))?;
        pool.counters.created.fetch_add(1, Ordering::Relaxed);
        BufReader::new(stream)
    };
    let result=async {
        connection.write_all(format!("GET / HTTP/1.1\r\nHost: server\r\nx-bench-client: {identity}\r\nConnection: keep-alive\r\n\r\n").as_bytes()).await.map_err(|e|ClientError::io("write",e))?;
        let mut line=String::new();
        if connection.read_line(&mut line).await.map_err(|e|ClientError::io("read_status",e))?==0 {return Err(ClientError::protocol("read_status","eof"));}
        if line.len()>8192 {return Err(ClientError::protocol("read_status","oversized"));}
        let status=line.split_whitespace().nth(1).and_then(|v|v.parse::<u16>().ok()).filter(|s|(200..=599).contains(s)).ok_or_else(||ClientError::protocol("read_status","invalid_status"))?;
        let (mut length,mut chunked,mut close)=(None,false,false);let mut headers=line.len();
        loop {
            line.clear();
            if connection.read_line(&mut line).await.map_err(|e|ClientError::io("read_headers",e))?==0 {return Err(ClientError::protocol("read_headers","eof"));}
            headers+=line.len();if headers>65536 {return Err(ClientError::protocol("read_headers","oversized"));}
            if line=="\r\n" {break;}
            if let Some((key,value))=line.split_once(':') {
                match key.to_ascii_lowercase().as_str() {
                    "content-length"=>length=Some(value.trim().parse::<usize>().map_err(|_|ClientError::protocol("framing","invalid_length"))?),
                    "transfer-encoding"=>chunked=value.to_ascii_lowercase().contains("chunked"),
                    "connection"=>close=value.to_ascii_lowercase().contains("close"), _=>{}
                }
            }
        }
        if chunked {
            let mut total=0usize;
            loop {
                line.clear();
                if connection.read_line(&mut line).await.map_err(|e|ClientError::io("read_body",e))?==0 {return Err(ClientError::protocol("read_body","eof"));}
                let size=usize::from_str_radix(line.trim().split(';').next().unwrap_or(""),16).map_err(|_|ClientError::protocol("framing","invalid_chunk"))?;
                if size==0 {
                    loop {
                        line.clear();
                        if connection.read_line(&mut line).await.map_err(|e|ClientError::io("read_body",e))?==0 {return Err(ClientError::protocol("read_body","eof"));}
                        headers+=line.len();if headers>65536 {return Err(ClientError::protocol("read_body","oversized_trailers"));}
                        if line=="\r\n" {break;}
                    }
                    break;
                }
                total=total.checked_add(size).filter(|n|*n<=65536).ok_or_else(||ClientError::protocol("read_body","oversized"))?;
                let mut body=vec![0;size+2];connection.read_exact(&mut body).await.map_err(|e|ClientError::io("read_body",e))?;
                if body[size..]!=*b"\r\n" {return Err(ClientError::protocol("framing","invalid_chunk_terminator"));}
            }
        } else if let Some(size)=length {
            if size>65536 {return Err(ClientError::protocol("read_body","oversized"));}
            let mut body=vec![0;size];connection.read_exact(&mut body).await.map_err(|e|ClientError::io("read_body",e))?;
        } else if !matches!(status,204|304) {return Err(ClientError::protocol("framing","missing_length"));}
        Ok((status,close))
    }.await;
    let (status, close) = result.map_err(|mut error: ClientError| {
        error.reused = reused;
        error
    })?;
    if !close {
        pool.idle.lock().await.push_back(IdleConnection {
            connection,
            returned: Instant::now(),
        });
    }
    Ok(status)
}

struct Observation {
    client: usize,
    status: u16,
    latency: u64,
    lag: u64,
    end: Instant,
    error: Option<ClientError>,
    executor_wait: u64,
    submission_lag: u64,
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
    executor_wait: Histogram,
    submission_lag: Histogram,
    completed: u64,
    error_categories: std::collections::BTreeMap<String, u64>,
    error_examples: Vec<Value>,
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
            executor_wait: Histogram::new(),
            submission_lag: Histogram::new(),
            completed: 0,
            error_categories: Default::default(),
            error_examples: Vec::new(),
        }
    }
    fn add(&mut self, r: Observation, deadline: Instant, start: Instant) {
        let index = match r.status {
            200 => 0,
            429 => 1,
            503 => 2,
            0 => 4,
            _ => 3,
        };
        if let Some(error) = &r.error {
            *self.error_categories.entry(error.key()).or_default() += 1;
            if self.error_examples.len() < 16 {
                self.error_examples.push(json!({"phase_ms":r.end.saturating_duration_since(start).as_millis(),"stage":error.stage,"kind":error.kind,"reused_connection":if error.stage=="request" {Value::Null} else {json!(error.reused)}}));
            }
        }
        self.statuses[index] += 1;
        self.latency.add(r.latency);
        self.lag.add(r.lag);
        self.executor_wait.add(r.executor_wait);
        self.submission_lag.add(r.submission_lag);
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
    let pool: Pool = new_pool();
    let permits = Arc::new(Semaphore::new(concurrency));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let queued = Arc::new(AtomicUsize::new(0));
    let (mut peak_queued, mut peak_reserved) = (0, 0);
    let (mut bin_ms, mut bin_count, mut peak_dispatches) = (0, 0, 0);
    let mut dispatched = 0usize;
    let mut tasks = JoinSet::new();
    let mut results = Results::new(clients);
    let (mut late, mut capacity) = (0, 0);
    let scheduled = seconds * rate;
    for i in 0..scheduled {
        let due = start + Duration::from_secs_f64(i as f64 / rate as f64);
        sleep_until(due).await;
        // Bound tasks waiting for their first poll separately from active exchanges.
        // Keep the original due time: executor pressure remains visible in lag/loss.
        while queued.load(Ordering::Relaxed) >= QUEUED_TASK_LIMIT
            && Instant::now().saturating_duration_since(due) <= Duration::from_millis(100)
        {
            tokio::task::yield_now().await;
        }
        while let Some(result) = tasks.try_join_next() {
            results.add(result.expect("load worker panicked"), deadline, start);
        }
        if Instant::now().saturating_duration_since(due) > Duration::from_millis(100) {
            late += 1;
            continue;
        }
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            peak_reserved = concurrency;
            capacity += 1;
            continue;
        };
        let submitted = Instant::now();
        let ms = submitted.saturating_duration_since(start).as_millis();
        if ms != bin_ms {
            bin_ms = ms;
            bin_count = 0;
        }
        bin_count += 1;
        peak_dispatches = peak_dispatches.max(bin_count);
        peak_reserved = peak_reserved.max(concurrency - permits.available_permits());
        let queued_count = queued.fetch_add(1, Ordering::Relaxed) + 1;
        peak_queued = peak_queued.max(queued_count);
        let queued = queued.clone();
        let (pool, active, peak) = (pool.clone(), active.clone(), peak.clone());
        let client = i as usize % clients;
        let identity = format!("{prefix}-{client}");
        tasks.spawn(async move {
            let _permit = permit;
            let begin = Instant::now();
            queued.fetch_sub(1, Ordering::Relaxed);
            let current = active.fetch_add(1, Ordering::Relaxed) + 1;
            peak.fetch_max(current, Ordering::Relaxed);
            let (status, error) = match tokio::time::timeout(
                Duration::from_secs(3),
                exchange(pool, identity),
            )
            .await
            {
                Ok(Ok(status)) => (status, None),
                Ok(Err(error)) => (0, Some(error)),
                Err(_) => (0, Some(ClientError::protocol("request", "timeout"))),
            };
            let end = Instant::now();
            active.fetch_sub(1, Ordering::Relaxed);
            Observation {
                client,
                status,
                latency: (end - begin).as_micros() as u64,
                lag: begin.saturating_duration_since(due).as_micros() as u64,
                end,
                error,
                executor_wait: begin.saturating_duration_since(submitted).as_micros() as u64,
                submission_lag: submitted.saturating_duration_since(due).as_micros() as u64,
            }
        });
        dispatched += 1;
        // Let ready socket tasks run during catch-up, instead of reserving the whole cap.
        if dispatched % QUEUED_TASK_LIMIT == 0 {
            tokio::task::yield_now().await;
        }
    }
    sleep_until(deadline).await;
    while let Some(result) = tasks.join_next().await {
        results.add(result.expect("load worker panicked"), deadline, start);
    }
    let elapsed = (Instant::now() - start).as_secs_f64().max(seconds as f64);
    let divisor = elapsed.max(0.000001);
    let mut r = json!({"schema_version":3,"generator":"rust-open-loop","histogram_precision":"upper bounds: 0.01ms to 100ms; 0.1ms to 1s; 1ms thereafter",
        "target_rps":rate,"offered_rps":if seconds>0 {results.count as f64/seconds as f64} else {0.0},"achieved_rps":results.count as f64/divisor,
        "elapsed_seconds":elapsed,"measurement_seconds":seconds,"drain_seconds":(elapsed-seconds as f64).max(0.0),"scheduled":scheduled,"sample_count":results.count,
        "completed_in_window":results.completed,"completion_rps_in_window":if seconds>0 {results.completed as f64/seconds as f64} else {0.0},
        "dropped":late+capacity,"dropped_late":late,"dropped_capacity":capacity,"loadgen_saturated":late+capacity>0,
        "concurrency_limit":concurrency,"peak_in_flight":peak.load(Ordering::Relaxed),"mean_in_flight":results.latency_sum as f64/1e6/divisor,
        "http_200":results.statuses[0],"http_429":results.statuses[1],"http_503":results.statuses[2],"http_other":results.statuses[3],"errors":results.statuses[4],"error_categories":results.error_categories,"error_examples":results.error_examples,"connection_pool":pool.json(),
        "http_rps":(results.count-results.statuses[4]) as f64/divisor,"admitted_rps":results.statuses[0] as f64/divisor,
        "max_admissions_per_client":results.admitted.iter().max().unwrap_or(&0),"expected_admissions":results.attempts.iter().map(|n|(*n).min(limit)).sum::<usize>(),
        "clients_observed":results.attempts.iter().filter(|n|**n>0).count(),"dispatch_lag":results.lag.json(),"scheduled_latency":results.scheduled_latency.json(),
        "status_latency":{"200":results.status_latency[0].json(),"429":results.status_latency[1].json(),"503":results.status_latency[2].json()}});
    let dispatch = json!({"queued_task_limit":QUEUED_TASK_LIMIT,"peak_reserved_slots":peak_reserved,"peak_queued_tasks":peak_queued,"peak_dispatches_per_1ms_bin":peak_dispatches,"executor_wait":results.executor_wait.json(),"submission_lag":results.submission_lag.json()});
    r.as_object_mut()
        .unwrap()
        .extend(dispatch.as_object().unwrap().clone());
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
fn phase_barrier(event: &str) {
    if number("PHASE_BARRIER", 0) == 1 {
        println!("{}", json!({"event":event}));
        io::stdout().flush().expect("flush phase event");
        let mut line = String::new();
        assert!(
            io::stdin().read_line(&mut line).unwrap() > 0,
            "phase controller disconnected"
        );
        assert_eq!(line.trim(), event, "unexpected phase acknowledgement");
    }
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
    let pool = new_pool();
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
    phase_barrier("measurement_ready");
    let before = cgroup();
    let mut result = phase(duration, rate, concurrency, clients, "measure", limit).await;
    result["loadgen_cgroup_before"] = before;
    result["loadgen_cgroup"] = cgroup();
    phase_barrier("measurement_complete");
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
        assert_eq!(r["peak_reserved_slots"], 0);
        assert_eq!(r["peak_queued_tasks"], 0);
        assert_eq!(r["peak_dispatches_per_1ms_bin"], 0);
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
        let pool = new_pool();
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
    async fn read_request(connection: &mut Connection) {
        loop {
            let mut line = String::new();
            assert!(connection.read_line(&mut line).await.unwrap() > 0);
            if line == "\r\n" {
                break;
            }
        }
    }
    #[tokio::test]
    async fn idle_connection_after_five_seconds_is_replaced_without_request_replay() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let task = tokio::spawn(async move {
            for index in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                read_request(&mut stream).await;
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await
                    .unwrap();
                // Match the default server idle keep-alive lifetime.
                if index == 0 {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        });
        let pool = new_pool();
        assert_eq!(
            exchange_at(pool.clone(), "first".into(), &address)
                .await
                .unwrap(),
            200
        );
        tokio::time::sleep(Duration::from_millis(5100)).await;
        assert_eq!(
            exchange_at(pool.clone(), "second".into(), &address)
                .await
                .unwrap(),
            200
        );
        task.await.unwrap();
        assert_eq!(pool.counters.created.load(Ordering::Relaxed), 2);
        assert_eq!(pool.counters.expired.load(Ordering::Relaxed), 1);
    }
    #[tokio::test]
    async fn idle_remote_eof_is_discarded_before_new_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let task = tokio::spawn(async move {
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                read_request(&mut stream).await;
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await
                    .unwrap();
            }
        });
        let pool = new_pool();
        exchange_at(pool.clone(), "first".into(), &address)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        exchange_at(pool.clone(), "second".into(), &address)
            .await
            .unwrap();
        task.await.unwrap();
        assert_eq!(pool.counters.closed.load(Ordering::Relaxed), 1);
    }
    #[tokio::test]
    async fn eof_after_request_write_is_classified_and_never_retried() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            read_request(&mut stream).await;
        });
        let pool = new_pool();
        let error = exchange_at(pool.clone(), "uncertain-admission".into(), &address)
            .await
            .unwrap_err();
        task.await.unwrap();
        assert_eq!(error.key(), "read_status:eof");
        assert_eq!(pool.counters.created.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn error_details_are_bounded_but_counts_are_complete() {
        let start = Instant::now();
        let mut results = Results::new(1);
        for _ in 0..20 {
            results.add(
                Observation {
                    client: 0,
                    status: 0,
                    latency: 1,
                    lag: 0,
                    end: start,
                    error: Some(ClientError::protocol("read_status", "eof")),
                    executor_wait: 0,
                    submission_lag: 0,
                },
                start,
                start,
            );
        }
        assert_eq!(results.error_categories["read_status:eof"], 20);
        assert_eq!(results.error_examples.len(), 16);
        assert_eq!(results.statuses[4], 20);
    }
}
