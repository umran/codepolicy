// `good` opens a transaction and commits it; `bad` forgets to.

export function good() {
  const tx = db.begin();
  doWork();
  tx.commit();
}

export function bad() {
  const tx = db.begin();
  doWork();
  // no commit() or rollback() before the function ends
}
