var obj = { a: 1, b: 2, c: 3, d: 4 };
var json = "";
for (var i = 0; i < 1000; i = i + 1) {
  json = JSON.stringify(obj);
}
