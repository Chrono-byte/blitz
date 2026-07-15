var arr = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
var sum = 0;
for (var i = 0; i < 1000; i = i + 1) {
  arr.forEach(function (v) {
    sum = sum + v;
  });
}
