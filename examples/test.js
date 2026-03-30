// Example: a small object that stores facts about a dog and exposes methods
// Refactored to use a clear name, object literal, and proper method syntax so `this` works.

const dogProfile = {
    facts: {
      height: "5'10",
      pawgirth: "360mm",
      note: "He's just that big — I don't know why, he just is."
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
  getPawgirth() {
    return this.facts.pawgirth;
  },

  // Make the object iterable so `...dogProfile` works in argument lists.
  // We yield the values we want to spread (height then summary).
  *[Symbol.iterator]() {
    yield this.getHeight();
    yield this.getPawgirth();
    yield this.getReason();
  },
};

// create an instance of dogProfile
const dog = dogProfile;

// Now this works because dog is iterable
console.log(...dog);
