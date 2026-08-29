import { useCallback, useMemo, useState } from "react";
import { Container } from "@/components/ui/container";
import { Empty, EmptyDescription, EmptyTitle } from "@/components/ui/empty";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import type { FetchFailure } from "@/lib/api";
import type { Document } from "@/lib/documents";
import { buildRepoTree } from "@/lib/tree";
import { useDocuments } from "@/hooks/use-documents";
import { AlertRegion } from "@/views/alert-region";
import { DocumentPane } from "@/views/document-pane";
import { DocumentTree } from "@/views/document-tree";
import { SearchBox } from "@/views/search-box";

/** The repo this app serves. */
const REPO = "legion";

/** Placeholder title a newly created intention opens with. */
const NEW_INTENTION_TITLE = "New intention";

function App() {
  const { documents, error, replaceDocument, setDocumentStatus, createDocument } = useDocuments();
  const [selection, setSelection] = useState<Document | null>(null);
  const [pending, setPending] = useState<Document | null | undefined>(undefined);
  const [actionFailure, setActionFailure] = useState<FetchFailure | null>(null);

  const tree = useMemo(() => buildRepoTree(REPO, documents), [documents]);

  // Prefer the loaded copy so a revision elsewhere is reflected here.
  const selected = selection
    ? (documents.find((doc) => doc.id === selection.id) ?? selection)
    : null;

  /** Switching documents runs past the pane's leave prompt first. */
  const requestSelect = useCallback(
    (next: Document) => {
      setActionFailure(null);
      if (selection && selection.id !== next.id) setPending(next);
      else setSelection(next);
    },
    [selection],
  );

  const confirmLeave = useCallback(() => {
    setSelection(pending ?? null);
    setPending(undefined);
  }, [pending]);

  const cancelLeave = useCallback(() => setPending(undefined), []);

  const handleCreateIntention = useCallback(() => {
    setActionFailure(null);
    void (async () => {
      // The tree shows a thesis by its title and falls back to the raw id, so a
      // new intention is seeded with one rather than opening as a UUID. The
      // endpoint accepts an empty payload and fills nothing in itself.
      const result = await createDocument("thesis", REPO, { title: NEW_INTENTION_TITLE });
      if (result.ok) requestSelect(result.document);
      else setActionFailure(result.failure);
    })();
  }, [createDocument, requestSelect]);

  return (
    <SidebarProvider>
      <DocumentTree
        tree={tree}
        selectedId={selected?.id ?? null}
        onSelect={requestSelect}
        onCreateIntention={handleCreateIntention}
      />

      <SidebarInset>
        {/* Container intel: "Use header with position=\"sticky\" and
            depth=\"navigation\" for site headers". The design asks for the
            search box pinned right, but no rafters part or Container prop
            places a child at the trailing edge, so it sits where the header
            puts it and the gap is filed rather than faked with a spacer. */}
        <Container as="header" position="sticky" depth="navigation">
          <SearchBox repo={REPO} onSelect={requestSelect} />
        </Container>

        <AlertRegion failures={[actionFailure, error]} />

        {selected ? (
          <DocumentPane
            key={selected.id}
            document={selected}
            documents={documents}
            pendingSelection={pending}
            onLeaveConfirmed={confirmLeave}
            onLeaveCancelled={cancelLeave}
            onDocumentChanged={replaceDocument}
            setDocumentStatus={setDocumentStatus}
          />
        ) : (
          <Empty>
            <EmptyTitle>No document selected</EmptyTitle>
            <EmptyDescription>
              Pick one from the tree, or search for it in the box above.
            </EmptyDescription>
          </Empty>
        )}
      </SidebarInset>
    </SidebarProvider>
  );
}

export default App;
