use std::{thread, time::Duration};

use futures::{stream::FuturesUnordered, StreamExt};
use rand::Rng;
use tl_async_runtime::{block_on, spawn, Sleep};

fn main() {
    block_on(async {
        let futs = FuturesUnordered::new();
        for i in 0..10 {
            futs.push(spawn(async move {
                let ms = rand::thread_rng().gen_range(0..5000);
                Sleep::duration(Duration::from_millis(ms)).await;
                print_from_thread(i).await;
            }));
        }
        futs.collect::<()>().await;
        print_from_thread(10).await;
    });
}

async fn print_from_thread(i: usize) {
    Sleep::duration(Duration::from_millis(10)).await;
    println!("Hi from inside future {}! {:?}", i, thread::current().id());
}
