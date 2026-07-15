// simple_demo.js
// A compact JavaScript language demo using only core language features
// and console.log for output. No Node.js or browser-specific APIs are used.

// 1) Variables and basic types
const Name = "Ada";
let age = 30;
var isActive = true; // old-style declaration (not recommended for new code)

console.log("--- Basics ---");
console.log("name:", Name);
console.log("age:", age);
console.log("isActive:", isActive);

// 2) Functions: declaration, expression, and arrow function
function greet(person = "friend") {
  return `Hello, ${person}!`;
}

const farewell = function (person) {
  return "Goodbye, " + person + ".";
};

const add = (a, b) => a + b;

console.log("\n--- Functions ---");
console.log(greet(Name));
console.log(farewell(Name));
console.log("add(2,3) =", add(2, 3));

// 3) Rest parameters, default values, and spread
function sum(...numbers) {
  return numbers.reduce((acc, n) => acc + n, 0);
}

console.log("\n--- Rest / Spread ---");
console.log("sum(1,2,3,4) =", sum(1, 2, 3, 4));

const base = [1, 2, 3];
// Use concat instead of spread for compatibility with runtimes that
// may not fully support array spread.
const extended = base.concat(4, 5);
console.log("extended array:", extended);

// 4) Arrays: map, filter, reduce
const numbers = [1, 2, 3, 4, 5];
const squares = numbers.map((n) => n * n);
const evens = numbers.filter((n) => n % 2 === 0);
const total = numbers.reduce((a, b) => a + b, 0);

console.log("\n--- Arrays ---");
console.log("numbers:", numbers);
console.log("squares:", squares);
console.log("evens:", evens);
console.log("sum of numbers:", total);

// 5) Objects and destructuring
const person = {
  firstName: "Grace",
  lastName: "Hopper",
  role: "Engineer",
  // method using `this`
  fullName() {
    return `${this.firstName} ${this.lastName}`;
  },
};

// Use property access instead of destructuring for compatibility with
// runtimes that may have partial destructuring support.
const firstName = person.firstName;
const role = person.role;
console.log("\n--- Objects ---");
console.log("person.fullName():", person.fullName());
console.log("destructured firstName:", firstName);
console.log("destructured role:", role);

// 6) Closures: a simple counter factory
function createCounter(start = 0) {
  let count = start;
  // returned function closes over `count`
  return function increment() {
    count += 1;
    return count;
  };
}

console.log("\n--- Closures ---");
const counter = createCounter(10);
console.log("counter():", counter());
console.log("counter():", counter());

// 7) Higher-order function: run a function multiple times
function times(n, fn) {
  for (let i = 0; i < n; i++) fn(i);
}

console.log("\n--- Higher-order Function ---");
times(3, (i) => console.log(`times callback ${i}`));

// 8) Recursion: factorial (simple, synchronous)
function factorial(n) {
  if (n < 0) throw new Error("factorial: negative input");
  return n <= 1 ? 1 : n * factorial(n - 1);
}

console.log("\n--- Recursion ---");
console.log("factorial(0) =", factorial(0));
console.log("factorial(5) =", factorial(5));

// 9) Simple error handling with try/catch
console.log("\n--- Error Handling ---");
try {
  // cause an error on purpose
  factorial(-1);
} catch (err) {
  console.log("caught error:", err.message);
}

// 10) Ternary operator and template literals
const Status = age >= 18 ? "adult" : "minor";
console.log("\n--- Misc ---");
console.log(`${Name} is an ${Status}.`);

// End of demo
console.log("\n--- Demo complete ---");

// 11) Test modern syntax using eval + try/catch
// Use eval so parsing of modern syntax can be caught at runtime in older engines
console.log("\n--- Modern Feature Tests (eval + try/catch) ---");

try {
  // Prefer a literal spread test inside eval. If the runtime mis-executes spread,
  // fall back to concat so the demo still shows the intended result.
  // eslint-disable-next-line no-eval
  const modernSpread = eval("[...[1, 2, 3], 4, 5]");
  if (
    modernSpread &&
    modernSpread.length === 5 &&
    modernSpread[0] === 1 &&
    modernSpread[1] === 2 &&
    modernSpread[2] === 3 &&
    modernSpread[3] === 4 &&
    modernSpread[4] === 5
  ) {
    console.log("modern spread result:", modernSpread);
  } else {
    console.log("modern spread result:", base.concat(4, 5));
    console.log(
      "modern spread note:",
      "eval spread produced incorrect runtime values; used concat fallback",
    );
  }
} catch (err) {
  console.log("modern spread result:", base.concat(4, 5));
  console.log("modern spread failed:", err && err.message ? err.message : String(err));
}

try {
  // Make the object visible to eval through the global object so lexical-scope
  // limitations in the runtime do not interfere with testing destructuring syntax.
  globalThis.person = person;
  // eslint-disable-next-line no-eval
  const destructRes = eval(
    "(function(){ const { firstName: f, role: r } = globalThis.person; return [f, r]; })()",
  );
  console.log("modern destructuring result:", destructRes);
} catch (err) {
  console.log("modern destructuring failed:", err && err.message ? err.message : String(err));
}

console.log("\n--- Modern Feature Tests Complete ---");
