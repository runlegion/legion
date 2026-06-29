// Hello world. One self-registering web component, just enough to prove the
// pipeline: TS source -> build -> embedded in the legion binary -> served.
// rafters controls replace everything real from here.
class LegionHello extends HTMLElement {
  connectedCallback(): void {
    const h = document.createElement("h1");
    h.textContent = "legion dashboard - hello world";
    this.replaceChildren(h);
  }
}

customElements.define("legion-hello", LegionHello);
