// SPDX-License-Identifier: MPL-2.0
var deepObj = { a: 1 };
var current = deepObj;
for (var i = 0; i < 100; i = i + 1) {
  current.next = { value: i };
  current = current.next;
}
var sum = 0;
var node = deepObj;
for (var j = 0; j < 100; j = j + 1) {
  node = node.next;
  sum = sum + node.value;
}
