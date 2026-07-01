// Legion dashboard shell. A per-repo navigator over the document layer:
// repo -> { Service Design, Specifications, Kanban } -> doc -> render + Publish.
// Vanilla web component; rafters controls replace the hand-rolled bits (badges,
// buttons) once the rafters WC constitution proves out on a reference component.
import {
  type DocumentRow,
  getDocument,
  listDocuments,
  setDocumentStatus,
} from "./api.ts";
import { escapeHtml, renderDocument, sectionForType } from "./render.ts";

const PUBLISHED = "published";

interface State {
  docs: DocumentRow[];
  loading: boolean;
  error: string | null;
  openRepo: string | null;
  openSection: "design" | "spec" | "kanban" | null;
  selectedId: string | null;
  selected: DocumentRow | null;
  publishing: boolean;
}

function titleOf(row: DocumentRow): string {
  try {
    const p = JSON.parse(row.payload) as Record<string, unknown>;
    const t = (p.title as string) ?? (p.slug as string);
    if (t) return t;
  } catch {
    /* fall through to id */
  }
  return row.id;
}

class LegionApp extends HTMLElement {
  private state: State = {
    docs: [],
    loading: true,
    error: null,
    openRepo: null,
    openSection: null,
    selectedId: null,
    selected: null,
    publishing: false,
  };

  connectedCallback(): void {
    this.addEventListener("click", (e) => {
      void this.onClick(e);
    });
    void this.loadDocs();
  }

  private set(patch: Partial<State>): void {
    this.state = { ...this.state, ...patch };
    this.render();
  }

  private async loadDocs(): Promise<void> {
    try {
      const docs = await listDocuments();
      this.set({ docs, loading: false, error: null });
    } catch (err) {
      this.set({ loading: false, error: String(err) });
    }
  }

  private repos(): string[] {
    const set = new Set<string>();
    for (const d of this.state.docs) {
      if (d.surface) set.add(d.surface);
    }
    return [...set].sort();
  }

  private docsFor(repo: string, section: "design" | "spec"): DocumentRow[] {
    return this.state.docs
      .filter((d) => d.surface === repo)
      .filter((d) => {
        const s = sectionForType(d.doc_type);
        return section === "design" ? s === "design" : s === "spec";
      })
      .sort((a, b) => titleOf(a).localeCompare(titleOf(b)));
  }

  private async onClick(e: Event): Promise<void> {
    const el = (e.target as HTMLElement).closest("[data-act]");
    if (!el) return;
    const act = el.getAttribute("data-act");
    e.preventDefault();

    if (act === "repo") {
      const repo = el.getAttribute("data-repo");
      this.set({
        openRepo: this.state.openRepo === repo ? null : repo,
        openSection: null,
      });
    } else if (act === "section") {
      const section = el.getAttribute("data-section") as State["openSection"];
      this.set({
        openSection: this.state.openSection === section ? null : section,
      });
    } else if (act === "doc" || act === "ref") {
      const id = el.getAttribute("data-doc");
      if (id) await this.selectDoc(id);
    } else if (act === "publish") {
      await this.publish();
    }
  }

  private async selectDoc(id: string): Promise<void> {
    this.set({ selectedId: id, selected: null });
    try {
      const doc = await getDocument(id);
      this.set({ selected: doc });
    } catch (err) {
      this.set({ error: String(err) });
    }
  }

  private async publish(): Promise<void> {
    const cur = this.state.selected;
    if (!cur) return;
    this.set({ publishing: true });
    try {
      const updated = await setDocumentStatus(cur.id, PUBLISHED);
      // Reflect the new status in the cached list so the sidebar dot updates
      // without a full refetch.
      const docs = this.state.docs.map((d) =>
        d.id === updated.id ? { ...d, status: updated.status } : d,
      );
      this.set({ selected: updated, docs, publishing: false });
    } catch (err) {
      this.set({ error: String(err), publishing: false });
    }
  }

  private render(): void {
    this.innerHTML = `
      <div class="layout">
        <aside class="sidebar">
          <div class="brand">legion</div>
          ${this.renderNav()}
        </aside>
        <main class="content">${this.renderContent()}</main>
      </div>`;
  }

  private renderNav(): string {
    if (this.state.loading) return `<div class="muted pad">loading…</div>`;
    if (this.state.error) {
      return `<div class="error pad">${escapeHtml(this.state.error)}</div>`;
    }
    const repos = this.repos();
    if (!repos.length) return `<div class="muted pad">no documents</div>`;
    return `<nav class="tree">${repos.map((r) => this.renderRepo(r)).join("")}</nav>`;
  }

  private renderRepo(repo: string): string {
    const open = this.state.openRepo === repo;
    let sections = "";
    if (open) {
      sections = `<div class="sections">
        ${this.renderSection(repo, "design", "Service Design")}
        ${this.renderSection(repo, "spec", "Specifications")}
        <div class="section">
          <button class="section-toggle" data-act="section" data-section="kanban">Kanban</button>
          ${
            this.state.openSection === "kanban"
              ? `<div class="muted pad-sm">board view — wiring pending</div>`
              : ""
          }
        </div>
      </div>`;
    }
    return `<div class="repo ${open ? "open" : ""}">
      <button class="repo-toggle" data-act="repo" data-repo="${escapeHtml(repo)}">
        <span class="caret">${open ? "▾" : "▸"}</span>${escapeHtml(repo)}
      </button>
      ${sections}
    </div>`;
  }

  private renderSection(
    repo: string,
    section: "design" | "spec",
    label: string,
  ): string {
    const docs = this.docsFor(repo, section);
    const open = this.state.openSection === section;
    const items = open
      ? `<ul class="doc-list">${docs
          .map(
            (d) => `<li class="doc-item ${this.state.selectedId === d.id ? "sel" : ""}"
              data-act="doc" data-doc="${escapeHtml(d.id)}">
              <span class="doc-item-title">${escapeHtml(titleOf(d))}</span>
              <span class="status-dot ${d.status === PUBLISHED ? "pub" : "draft"}" title="${escapeHtml(d.status)}"></span>
            </li>`,
          )
          .join("")}</ul>`
      : "";
    return `<div class="section">
      <button class="section-toggle" data-act="section" data-section="${section}">
        ${escapeHtml(label)} <span class="count">${docs.length}</span>
      </button>
      ${items}
    </div>`;
  }

  private renderContent(): string {
    const { selectedId, selected, publishing } = this.state;
    if (!selectedId) {
      return `<div class="empty">Select a document from the left.</div>`;
    }
    if (!selected) return `<div class="muted pad">loading document…</div>`;

    const isPublished = selected.status === PUBLISHED;
    const publishBtn = `<button class="publish ${isPublished ? "done" : ""}"
      data-act="publish" ${isPublished || publishing ? "disabled" : ""}>
      ${isPublished ? "Published" : publishing ? "Publishing…" : "Publish"}
    </button>`;

    return `<div class="doc-view">
      <header class="doc-head">
        <div class="doc-meta"><span class="doc-id">${escapeHtml(selected.id)}</span>
          <span class="doc-type">${escapeHtml(selected.doc_type)}</span></div>
        ${publishBtn}
      </header>
      <div class="doc-body">${renderDocument(selected)}</div>
    </div>`;
  }
}

customElements.define("legion-app", LegionApp);
