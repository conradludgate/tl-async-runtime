pub mod io;
pub mod timers;

#[derive(Default)]
pub struct Reactor {
    os: io::Os,
    timers: timers::Queue,
}

impl Reactor {
    // this is run by any thread that currently is not busy.
    // It manages the timers and OS polling in order to wake up tasks
    pub fn book_keeping(&self) -> usize {
        let mut n = 0;
        // get the current task timers that have elapsed and insert them into the ready tasks
        for task in &self.timers {
            task.wake();
            n += 1;
        }

        // get the OS events
        n + self.os.process()
    }
}
