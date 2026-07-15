var obj = { x: 1, y: 2 };
var sum = 0;
for (var i = 0; i < 5000; i = i + 1) {
  obj.x = obj.x + 1;
  sum = sum + obj.x + obj.y;
}
