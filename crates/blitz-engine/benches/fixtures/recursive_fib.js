// SPDX-License-Identifier: MPL-2.0
function fib(n) {
  if (n <= 1) {
    return n;
  }
  return fib(n - 1) + fib(n - 2);
}
var sum = 0;
for (var i = 0; i < 100; i = i + 1) {
  sum = sum + fib(15);
}
