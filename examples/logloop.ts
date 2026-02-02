// Microbenchmark: a tight loop that calls console.log repeatedly to measure
// runtime overhead on the hot path. Adjust `ITER` if you want longer runs.
const ITER = 10000;
let i = 0;
while (i < ITER) {
  console.log("x");
  i = i + 1;
}

// Print a small marker so tracing output is easier to grep for.
console.log("done");
