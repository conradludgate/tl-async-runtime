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
gets the thread-local executor object and pushes a `(Time, TaskId)` pair
into a priority queue (ordered by time ascending).

The book-keepers will loop through this priority queue and send the respective task IDs
into the ready queue.

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
    Latency     2.50ms  160.95us  12.45ms   95.11%
    Req/Sec     1.73k     0.99k    3.10k    37.17%
  103290 requests in 20.05s, 14.97MB read
Requests/sec:   5150.41
Transfer/sec:    764.51KB
```

#### Tokio
```
Running 20s test @ http://localhost:8080/json
  12 threads and 500 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    95.07ms  152.83ms   1.93s    85.96%
    Req/Sec     4.58k     2.48k    8.91k    62.16%
  1084726 requests in 20.05s, 157.24MB read
Requests/sec:  54101.15
Transfer/sec:      7.84MB
```

### Single threaded

#### tl-async-runtime
```
Running 20s test @ http://localhost:8080/json
  12 threads and 500 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency     4.01ms  314.68us  30.34ms   91.31%
    Req/Sec     1.43k   761.11     3.40k    49.88%
  114121 requests in 20.06s, 16.54MB read
Requests/sec:   5688.68
Transfer/sec:    844.41KB
```

#### Tokio
```
Running 20s test @ http://localhost:8080/json
  12 threads and 500 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    41.82ms    1.33ms  60.03ms   93.04%
    Req/Sec     0.98k   122.88     1.24k    58.54%
  235139 requests in 20.08s, 34.09MB read
Requests/sec:  11711.46
Transfer/sec:      1.70MB
```

### Conclusion

Tokio's single-threaded performance has 2x the throughput, and 10x the throughput when multi-threaded.
It also has 10x the single-threaded latency and 50x when multi-threaded.

This is expected. Using [lines of code](https://gist.github.com/conradludgate/417ef86f1764b41606f400de247692bf) as an estimate for complexity, tokio is ~60x more complex.
This would account for the longer latencies of bookkeeping and a more highly tuned runtime to support more req/s.

I consider this to be a success.
We have created a runtime within an order of magnitude of tokio,
while significantly simpler and easier to understand.

Downside, this runtime seemingly is struggling to make good use of multiple threads.
This may be a result of lock contensions as a result of not optimising the data structures.

### Alternatives

When benchmarking a non-async version that uses an OS thread per active connection seems surprisingly effective on my 8-core Linux machine.

```
Running 20s test @ http://localhost:8080/json
  12 threads and 500 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    21.50ms   18.68ms 220.64ms   75.76%
    Req/Sec   135.40k    46.30k  229.77k    68.06%
  32206727 requests in 20.13s, 4.56GB read
Requests/sec: 1599900.52
Transfer/sec:    231.92MB
```
