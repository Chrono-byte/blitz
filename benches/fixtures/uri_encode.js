var total = 0;
var input = "héllo/?=+&%#[]";
for (var i = 0; i < 20000; i = i + 1) {
  total = total + encodeURI(input).length;
  total = total + encodeURIComponent(input).length;
}
