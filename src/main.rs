use std::{thread, time::Duration};

use tl_async_runtime::{block_on, spawn, Sleep};

fn main() {
    block_on(async {
        println!("begin dispatch");
        for i in 0..6 {
            spawn(print_from_thread(i));
        }
        Sleep::duration(Duration::from_millis(1000)).await;
        println!("begin dispatch");
        for i in 6..12 {
            spawn(print_from_thread(i));
        }
        Sleep::duration(Duration::from_millis(1000)).await;
        println!("begin dispatch");
        for i in 12..18 {
            spawn(print_from_thread(i));
        }
        Sleep::duration(Duration::from_millis(1000)).await;
        println!("begin dispatch");
        print_from_thread(20).await;
    });
}

async fn print_from_thread(i: usize) {
    Sleep::duration(Duration::from_millis(10)).await;
    println!("Hi from inside future {}! {:?}", i, thread::current().id());
}
