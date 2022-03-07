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
    Latency    74.78ms   14.82ms 173.46ms   59.86%
    Req/Sec   117.42    109.90   404.00     76.17%
  25755 requests in 20.10s, 3.73MB read
Requests/sec:   1281.44
Transfer/sec:    190.21KB
```

#### Tokio
```
Running 20s test @ http://localhost:8080/json
  12 threads and 500 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    78.58ms   19.57ms 252.46ms   75.52%
    Req/Sec   527.69     72.58   747.00     88.10%
  125196 requests in 20.09s, 18.15MB read
Requests/sec:   6231.95
Transfer/sec:      0.90MB
```

### Single threaded

#### tl-async-runtime
```
Running 20s test @ http://localhost:8080/json
  12 threads and 500 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    75.63ms   16.24ms 199.04ms   63.16%
    Req/Sec   282.34    112.53   707.00     70.57%
  61703 requests in 20.07s, 8.94MB read
Requests/sec:   3074.14
Transfer/sec:    456.32KB
```

#### Tokio
```
Running 20s test @ http://localhost:8080/json
  12 threads and 500 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    76.28ms   15.26ms 185.57ms   60.50%
    Req/Sec   539.62     52.73   780.00     90.20%
  128620 requests in 20.08s, 18.64MB read
Requests/sec:   6406.56
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
    Latency   140.12ms  122.82ms 780.91ms   16.68%
    Req/Sec     3.20k     2.30k   18.82k    68.92%
  750797 requests in 20.07s, 108.83MB read
Requests/sec:  37408.86
Transfer/sec:      5.42MB
```
