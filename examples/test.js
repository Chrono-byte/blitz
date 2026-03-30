// Example: a small object that stores facts about a dog and exposes methods
// Refactored to use a clear name, object literal, and proper method syntax so `this` works.

const dogProfile = {
  facts: {
    height: "5'10",
    name: "Terry",
    note: "He's just that big; I don't know why, he just is."
  },

  // Method that uses `this` to access object data.
  // Using method shorthand ensures `this` refers to `dogProfile`.
  getHeight() {
    return this.facts.height;
  },

  // Composed helper method that returns a readable summary.
  getReason() {
    return this.facts.note;
  },

  // Method that uses `this` to access object data.
  // Using method shorthand ensures `this` refers to `dogProfile`.
  getName() {
    return this.facts.name;
  },

  // Make the object iterable so `...dogProfile` works in argument lists.
  // We yield the values we want to spread (height then summary).
  *[Symbol.iterator]() {
    yield ['height', this.getHeight()];
    yield ['name', this.getName()];
    yield ['note', this.getReason()];
  }
};

// Now this works because dogProfile is iterable
console.log(...dogProfile);
