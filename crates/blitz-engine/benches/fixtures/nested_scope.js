function outerScope() {
  var x = 1;
  var y = 2;
  var z = 3;
  function innerScope() {
    var sum = 0;
    for (var i = 0; i < 5000; i = i + 1) {
      sum = sum + x + y + z;
    }
    return sum;
  }
  return innerScope();
}
var result = outerScope();
