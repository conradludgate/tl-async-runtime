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
    Latency    75.04ms   15.04ms 165.93ms   59.90%
    Req/Sec   108.06     86.70   340.00     46.55%
  15049 requests in 20.03s, 2.18MB read
Requests/sec:    751.32
Transfer/sec:    111.52KB
```

#### Tokio
```
Running 20s test @ http://localhost:8080/json
  12 threads and 500 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    76.97ms   16.91ms 229.32ms   66.45%
    Req/Sec   536.28     67.71   787.00     92.65%
  127510 requests in 20.15s, 18.48MB read
Requests/sec:   6329.32
Transfer/sec:      0.92MB
```

### Single threaded

#### tl-async-runtime
```
Running 20s test @ http://localhost:8080/json
  12 threads and 500 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    74.81ms   14.61ms 149.33ms   57.70%
    Req/Sec   280.34    157.76   757.00     61.96%
  67171 requests in 20.10s, 9.74MB read
Requests/sec:   3342.51
Transfer/sec:    496.15KB
```

#### Tokio
```
Running 20s test @ http://localhost:8080/json
  12 threads and 500 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    76.06ms   14.88ms 185.92ms   59.61%
    Req/Sec   540.69     44.05   707.00     82.12%
  129271 requests in 20.07s, 18.74MB read
Requests/sec:   6441.72
Transfer/sec:      0.93MB
```

### Conclusion

Tokio's has a similar latency and roughly 2x the throughput on the single threaded executor

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
    Latency   136.41ms  121.26ms 601.08ms   15.52%
    Req/Sec     3.37k     2.17k   11.65k    63.80%
  798069 requests in 20.07s, 115.69MB read
Requests/sec:  39763.05
Transfer/sec:      5.76MB
```
