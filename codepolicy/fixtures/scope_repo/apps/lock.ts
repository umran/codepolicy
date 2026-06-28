export function safe() {
  acquire();
  doWork();
  release();
}

export function leaky() {
  acquire();
  doWork();
}
