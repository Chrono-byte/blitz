// SPDX-License-Identifier: MPL-2.0
function capture(value) {
  return function () {
    return value;
  };
}

var closures = [];
for (var i = 0; i < 5000; i = i + 1) {
  closures.push(capture(i));
}

var sum = 0;
for (var i = 0; i < closures.length; i = i + 1) {
  sum = sum + closures[i]();
}
if (sum !== 12497500) {
  throw new Error("captured values changed");
}
