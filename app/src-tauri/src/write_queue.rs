//! A single, serialized write-ahead queue for the SQLite writer connection
//! (SPEC.md S9, "Write Architecture & Contention").
//!
//! All writers - UI edits, background hashing, enrichment, (later) sync
//! ingestion - funnel through one dedicated thread that owns the single
//! writer `Connection`, so SQLite's single-writer constraint is respected by
//! construction instead of assumed away. Two priority lanes, mpsc-based:
//!
//! - `submit_immediate` - UI edits; always drained ahead of the background lane.
//! - `submit_background` - hashing/enrichment/sync batches; lower priority.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use rusqlite::Connection;

/// A unit of work executed against the single writer connection.
pub type Job = Box<dyn FnOnce(&Connection) + Send + 'static>;

/// Handle for submitting jobs to the write queue. Cheap to clone (it is just
/// two channel senders), so Tauri commands can hold their own copy in
/// managed state.
#[derive(Clone)]
pub struct WriteQueue {
    immediate_tx: Sender<Job>,
    background_tx: Sender<Job>,
}

enum Picked {
    Job(Job),
    Empty,
    Disconnected,
}

/// Picks the next job to run, always preferring the immediate lane over the
/// background lane. Kept as a free function so lane-priority ordering can be
/// unit tested directly, without a thread or a real database (see tests).
fn pick_next(immediate_rx: &Receiver<Job>, background_rx: &Receiver<Job>) -> Picked {
    match immediate_rx.try_recv() {
        Ok(job) => return Picked::Job(job),
        Err(TryRecvError::Empty) => {}
        // The immediate lane's sender was dropped; the background lane may
        // still have pending work, so fall through rather than stopping.
        Err(TryRecvError::Disconnected) => {}
    }

    match background_rx.try_recv() {
        Ok(job) => Picked::Job(job),
        Err(TryRecvError::Empty) => Picked::Empty,
        Err(TryRecvError::Disconnected) => Picked::Disconnected,
    }
}

impl WriteQueue {
    /// Spawns the dedicated writer thread that owns `conn` and starts
    /// draining both lanes, immediate-first. `conn` should already be opened
    /// and migrated (see `db::open`).
    pub fn start(conn: Connection) -> WriteQueue {
        let (immediate_tx, immediate_rx) = mpsc::channel::<Job>();
        let (background_tx, background_rx) = mpsc::channel::<Job>();

        thread::spawn(move || loop {
            match pick_next(&immediate_rx, &background_rx) {
                Picked::Job(job) => job(&conn),
                Picked::Disconnected => break,
                Picked::Empty => {
                    // Nothing to do; block briefly on the immediate lane so a
                    // fresh UI edit is picked up promptly without
                    // busy-spinning the thread.
                    match immediate_rx.recv_timeout(Duration::from_millis(25)) {
                        Ok(job) => job(&conn),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => {
                            if matches!(
                                pick_next(&immediate_rx, &background_rx),
                                Picked::Disconnected
                            ) {
                                break;
                            }
                        }
                    }
                }
            }
        });

        WriteQueue {
            immediate_tx,
            background_tx,
        }
    }

    /// Submits a UI-triggered edit. Always drained ahead of background work.
    pub fn submit_immediate(&self, job: Job) -> Result<(), &'static str> {
        self.immediate_tx
            .send(job)
            .map_err(|_| "write queue is shut down")
    }

    /// Submits background work (hashing, enrichment, sync ingestion). Runs
    /// whenever the immediate lane is empty.
    ///
    /// Not yet called by any command in this skeleton - `import_paths` is the
    /// only writer so far and is UI-triggered (immediate lane). Background
    /// hashing/enrichment land in a later milestone (SPEC.md Roadmap) and
    /// will be this lane's first real caller.
    #[allow(dead_code)]
    pub fn submit_background(&self, job: Job) -> Result<(), &'static str> {
        self.background_tx
            .send(job)
            .map_err(|_| "write queue is shut down")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn immediate_lane_is_drained_before_background_lane() {
        let (immediate_tx, immediate_rx) = mpsc::channel::<Job>();
        let (background_tx, background_rx) = mpsc::channel::<Job>();

        let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let record = |log: Arc<Mutex<Vec<&'static str>>>, label: &'static str| -> Job {
            Box::new(move |_conn: &Connection| log.lock().unwrap().push(label))
        };

        // Enqueue background jobs first, then an immediate job: arrival
        // order must not matter, only lane priority should.
        background_tx.send(record(log.clone(), "bg-1")).unwrap();
        background_tx.send(record(log.clone(), "bg-2")).unwrap();
        immediate_tx.send(record(log.clone(), "ui-1")).unwrap();

        let conn = Connection::open_in_memory().unwrap();

        // Drain deterministically without a background thread involved.
        while let Picked::Job(job) = pick_next(&immediate_rx, &background_rx) {
            job(&conn);
        }

        assert_eq!(*log.lock().unwrap(), vec!["ui-1", "bg-1", "bg-2"]);
    }

    #[test]
    fn a_later_immediate_job_still_jumps_ahead_of_queued_background_jobs() {
        let (immediate_tx, immediate_rx) = mpsc::channel::<Job>();
        let (background_tx, background_rx) = mpsc::channel::<Job>();

        let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let record = |log: Arc<Mutex<Vec<&'static str>>>, label: &'static str| -> Job {
            Box::new(move |_conn: &Connection| log.lock().unwrap().push(label))
        };

        background_tx.send(record(log.clone(), "bg-1")).unwrap();
        // Simulate the picker being consulted once before the UI edit
        // arrives (e.g. the writer thread was mid-loop): only "bg-1" is
        // available yet, so it is correctly picked.
        let conn = Connection::open_in_memory().unwrap();
        match pick_next(&immediate_rx, &background_rx) {
            Picked::Job(job) => job(&conn),
            _ => panic!("expected bg-1"),
        }

        background_tx.send(record(log.clone(), "bg-2")).unwrap();
        immediate_tx.send(record(log.clone(), "ui-1")).unwrap();

        while let Picked::Job(job) = pick_next(&immediate_rx, &background_rx) {
            job(&conn);
        }

        assert_eq!(*log.lock().unwrap(), vec!["bg-1", "ui-1", "bg-2"]);
    }

    #[test]
    fn write_queue_runs_submitted_jobs_on_its_writer_thread() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (v INTEGER)").unwrap();
        let queue = WriteQueue::start(conn);

        let (done_tx, done_rx) = mpsc::channel();
        queue
            .submit_immediate(Box::new(move |conn| {
                conn.execute("INSERT INTO t (v) VALUES (1)", []).unwrap();
                done_tx.send(()).unwrap();
            }))
            .unwrap();

        done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("write job did not complete in time");
    }
}
