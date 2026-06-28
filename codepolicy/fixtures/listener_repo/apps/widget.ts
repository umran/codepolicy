function setup() {
  el.addEventListener("click", onClick);
  el.removeEventListener("click", onClick);   // same object + event -> ok
}

function leaky() {
  el.addEventListener("scroll", onScroll);          // never removed -> leak
  el.addEventListener("resize", onResize);
  window.removeEventListener("resize", onResize);   // wrong object -> still a leak
}
