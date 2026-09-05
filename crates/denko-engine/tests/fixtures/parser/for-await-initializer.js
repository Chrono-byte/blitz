// Intentionally invalid: a for-await declaration cannot have an initializer.
async function invalid() { for await (var item = 1 of []) {} }
