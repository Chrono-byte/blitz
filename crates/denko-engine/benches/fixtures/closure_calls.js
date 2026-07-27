// SPDX-License-Identifier: MPL-2.0
function makeCounter(start) {
  var value = start;
  return function step() {
    value = value + 1;
    return value;
  };
}

var next = makeCounter(0);
var sum = 0;
for (var i = 0; i < 3000; i = i + 1) {
  sum = sum + next();
}
