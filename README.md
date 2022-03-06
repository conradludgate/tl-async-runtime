# tl-async-runtime

A fairly basic runtime example in <600 lines of logic

## Features

* Single threaded async
* Multi threaded async
* Timer support
* Basic IO (only TCP for now)

## Bugs

Many.

## Architecure

The runtime is first created by calling the `block_on` function, passing in a future.
The runtime will start up the threads and spawns the given future on the runtime.

At this point, all threads are 'workers'. Workers do a few things

1. The thread run some basic book-keeping tasks (more on this later).
2. Request a ready task from the runtime
    * If it can get a task, it will poll it once
    * If it can't get a task, it will 'park' the thread
3. Repeat

The main thread does 1 extra step, which is polling the channel future returned by the initial spawn.
This is so that we can exit as soon as that task has been completed.

### Bookkeeping

The core of the async runtime is scheduling tasks and managing the threads.

There's only 2 steps to this:
1. Looping through elapsed timers and putting them in a ready state
2. Polling the OS for events and dispatching them

### Timers

The timers are very rudementary. When a timer is first polled, it
gets the thread-local executor object and pushes a `(Time, Waker)` pair
into a priority queue (ordered by time ascending).

The book-keepers will loop through this priority queue and call the respective wakers

### OS Events

Using [mio](https://crates.io/crates/mio), event sources are registered
with the OS when they are created.

When events are requested for a specific source, a `[RegistrationToken -> Sender]` entry is pushed into
a hashmap.

The book-keepers will poll the OS for new events. If a corresponding token is found in the hashmap,
it will send the event along the channel. Since it's using a future-aware channel, it will auto-wake
any tasks that are waiting for the event

## Benchmark

The [http-server](examples/http-server.rs) example was taken from [tokio's tinyhttp](https://github.com/tokio-rs/tokio/blob/e8ae65a697d04aa11d5587c45caf999cb3b7f36e/examples/tinyhttp.rs) example.
I ran both of them and performed the following [wrk](https://github.com/wg/wrk) benchmark:

```sh
wrk -t12 -c500 -d20s http://localhost:8080/json
```

I got the following results:

### Multi threaded

#### tl-async-runtime
```
Running 20s test @ http://localhost:8080/json
  12 threads and 500 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    74.62ms   14.46ms 101.35ms   57.84%
    Req/Sec   107.43     67.91   250.00     61.36%
  10708 requests in 20.07s, 1.55MB read
Requests/sec:    533.55
Transfer/sec:     79.20KB
```

#### Tokio
```
Running 20s test @ http://localhost:8080/json
  12 threads and 500 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency   126.05ms   43.62ms 274.81ms   57.92%
    Req/Sec   325.58     40.00   430.00     74.61%
  77921 requests in 20.07s, 11.30MB read
Requests/sec:   3882.07
Transfer/sec:    576.25KB
```

### Single threaded

#### tl-async-runtime
```
Running 20s test @ http://localhost:8080/json
  12 threads and 500 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    74.75ms   14.53ms 137.04ms   57.81%
    Req/Sec   137.25     83.36   340.00     67.56%
  21907 requests in 20.08s, 3.18MB read
Requests/sec:   1090.72
Transfer/sec:    161.90KB
```

#### Tokio
```
Running 20s test @ http://localhost:8080/json
  12 threads and 500 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency   126.14ms   43.27ms 220.68ms   57.62%
    Req/Sec   325.27     38.86   440.00     73.58%
  77761 requests in 20.04s, 11.27MB read
Requests/sec:   3879.42
Transfer/sec:    575.85KB
```

### Conclusion

Tokio's has a similar latency and roughly 4x the throughput

Tokio also has a similar multi-threaded vs single-threaded performance in this example.
This runtime seems to have a degraded performance when attempting to use multiple threads.
This may be a result of lock contensions as a result of not optimising the data structures.

Using [lines of code](https://gist.github.com/conradludgate/417ef86f1764b41606f400de247692bf) as an estimate for complexity, tokio is ~60x more complex.

I would consider this a decent attempt at a runtime. It's not the most effective but it certainly does good enough,
especially for the intended goal of simplicity.

### Alternatives

When benchmarking a non-async version that uses an OS thread per active connection seems surprisingly effective on my 8-core Linux machine.

```
Running 20s test @ http://localhost:8080/json
  12 threads and 500 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    61.15ms   41.57ms 389.98ms   35.04%
    Req/Sec    25.17k    15.52k   63.76k    59.01%
  6013517 requests in 20.06s, 0.85GB read
Requests/sec: 299715.59
Transfer/sec:     43.45MB
```
