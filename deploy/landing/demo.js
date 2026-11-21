(() => {
  const figure = document.querySelector(".demo");
  const replay = document.querySelector("#replay");
  const motion = window.matchMedia("(prefers-reduced-motion: reduce)");

  // The complete transcript is plain HTML. Animate only its visual presentation.
  function updateMotion() {
    replay.hidden = motion.matches;
    if (motion.matches) figure.classList.remove("preview", "replaying");
    else figure.classList.add("preview");
  }

  replay.addEventListener("click", () => {
    figure.classList.remove("preview", "replaying");
    void figure.offsetWidth; // Restart the finite animation, even on a second click.
    figure.classList.add("replaying");
  });
  motion.addEventListener("change", updateMotion);
  updateMotion();
})();
