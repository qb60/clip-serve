use std::{
    io,
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
};

use clipserve::{capture::Capture, store::StoredImage};

use crate::platform::clipboard::{Data, read_image, sequence};

enum Job {
    Capture(Capture),
    Restore(u32, Arc<[u8]>),
}

pub enum Prepared {
    Capture(Capture, io::Result<StoredImage>),
    Restore(u32, io::Result<Data>),
}

#[derive(Default)]
struct Pending {
    capture: Option<Capture>,
    restore: Option<(u32, Arc<[u8]>)>,
    stop: bool,
}

pub struct Worker {
    inbox: Arc<(Mutex<Pending>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn start(send: impl Fn(Prepared) + Send + 'static) -> io::Result<Self> {
        let inbox = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
        let worker_inbox = Arc::clone(&inbox);
        let thread = thread::Builder::new()
            .name("Clipboard images".into())
            .spawn(move || {
                let (pending, ready) = &*worker_inbox;
                loop {
                    let job = {
                        let mut next = ready
                            .wait_while(pending.lock().unwrap(), |next| {
                                !next.stop && next.restore.is_none() && next.capture.is_none()
                            })
                            .unwrap();
                        if next.stop {
                            break;
                        }
                        if let Some((sequence, png)) = next.restore.take() {
                            Job::Restore(sequence, png)
                        } else {
                            Job::Capture(next.capture.take().unwrap())
                        }
                    };
                    match job {
                        Job::Capture(job) => {
                            if let Some(result) = read_image(job.sequence) {
                                send(Prepared::Capture(job, result));
                            }
                        }
                        Job::Restore(expected, png) if sequence() == expected => {
                            send(Prepared::Restore(expected, Data::image(&png)));
                        }
                        Job::Restore(..) => {}
                    }
                }
            })?;
        Ok(Self {
            inbox,
            thread: Some(thread),
        })
    }

    pub fn capture(&self, job: Capture) {
        let (pending, ready) = &*self.inbox;
        pending.lock().unwrap().capture = Some(job);
        ready.notify_one();
    }

    pub fn restore(&self, sequence: u32, png: Arc<[u8]>) {
        let (pending, ready) = &*self.inbox;
        pending.lock().unwrap().restore = Some((sequence, png));
        ready.notify_one();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let (pending, ready) = &*self.inbox;
        pending.lock().unwrap().stop = true;
        ready.notify_one();
        if self.thread.take().unwrap().join().is_err() {
            log::error!("The clipboard worker panicked.");
        }
    }
}
