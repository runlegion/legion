import { useCallback, useEffect, useState } from "react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Container } from "@/components/ui/container";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Editor } from "@/components/ui/editor";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import type { FetchFailure } from "@/lib/api";
import type { Document } from "@/lib/documents";
import { documentTitle, statusesForDocType } from "@/lib/tree";
import { useDocumentSave } from "@/hooks/use-document-save";
import type { MutationResult } from "@/hooks/use-documents";
import { AlertRegion } from "@/views/alert-region";
import { SettingsSheet } from "@/views/settings-sheet";

export interface DocumentPaneProps {
  document: Document;
  documents: Document[];
  /** A selection waiting on the leave prompt; undefined when none is pending. */
  pendingSelection: Document | null | undefined;
  onLeaveConfirmed: () => void;
  onLeaveCancelled: () => void;
  onDocumentChanged: (doc: Document) => void;
  setDocumentStatus: (id: string, status: string) => Promise<MutationResult>;
}

/**
 * One document: the toolbar above, the editor below. The pane is keyed on the
 * document id by its caller, so each document gets its own working copy.
 */
export function DocumentPane({
  document: doc,
  documents,
  pendingSelection,
  onLeaveConfirmed,
  onLeaveCancelled,
  onDocumentChanged,
  setDocumentStatus,
}: DocumentPaneProps) {
  const { flushPendingSave, revise, saving, error, hasUnrevisedEdits } = useDocumentSave(doc);
  const [actionFailure, setActionFailure] = useState<FetchFailure | null>(null);

  const statuses = statusesForDocType(documents, doc);
  const leavePending = pendingSelection !== undefined;

  const reviseNow = useCallback(
    async (patch?: Record<string, unknown>) => {
      const next = await revise(patch);
      onDocumentChanged(next);
    },
    [revise, onDocumentChanged],
  );

  const handleRevise = () => {
    setActionFailure(null);
    void reviseNow().catch(() => undefined);
  };

  /** Saves the status, then flushes the body save and cuts a revision. */
  const handleStatus = (status: string) => {
    setActionFailure(null);
    void (async () => {
      const result = await setDocumentStatus(doc.id, status);
      if (!result.ok) {
        setActionFailure(result.failure);
        return;
      }
      await flushPendingSave();
      await reviseNow().catch(() => undefined);
    })();
  };

  const handleSettingsSave = useCallback(
    async (patch: Record<string, unknown>) => {
      setActionFailure(null);
      await reviseNow(patch);
    },
    [reviseNow],
  );

  // Leaving with nothing unrevised needs no prompt.
  useEffect(() => {
    if (leavePending && !hasUnrevisedEdits) onLeaveConfirmed();
  }, [leavePending, hasUnrevisedEdits, onLeaveConfirmed]);

  const handleLeaveWithRevise = () => {
    void reviseNow()
      .then(() => onLeaveConfirmed())
      .catch(() => undefined);
  };

  return (
    <Container as="section">
      {/* No rafters React toolbar part exists -- `editor-toolbar` is a headless
          primitive (rendersForTarget: false) supplying button definitions, not
          a rendered bar -- so the toolbar is a plain Container and the gap is
          filed. No columns prop: placement is not ours to choose. */}
      <Container as="header">
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            {/* Button intel: "ghost/link/outline are the unlimited low-attention
                  register"; the one high-emphasis action in this view is the
                  dialog's Revise. */}
            <Button variant="outline">{doc.status}</Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent>
            {statuses.map((status) => (
              <DropdownMenuItem key={status} onSelect={() => handleStatus(status)}>
                {status}
              </DropdownMenuItem>
            ))}
            <DropdownMenuSeparator />
            <DropdownMenuItem onSelect={handleRevise}>Revise</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>

        <SettingsSheet document={doc} onSave={handleSettingsSave} saving={saving} />
      </Container>

      <AlertRegion failures={[actionFailure, error]} />

      {/*
        The rafters block editor. It mounts now that editor-history and ops/ are
        installed, but its React performance exposes no data binding: EditorProps
        is only {label|labelledBy, disabled?, readonly?}, and the component calls
        createEditorHistory() with no seed, ignoring the data-* seed path
        bindEditor uses for the Web Component and Astro performances. So the
        document body cannot be loaded into it and edits cannot be read out of
        it. Seeding it by hand would mean binding the contenteditable myself,
        which is both the hand-authored component the rafters rule forbids and
        the editor's own stated never. It renders readonly with the gap named
        until rafters ships a bound React surface.
      */}
      <Alert variant="warning">
        <AlertTitle>The editor is not bound to this document</AlertTitle>
        <AlertDescription>
          The rafters editor item takes no value and emits no change event on its React surface, so
          it opens empty for every document and shows nothing of this one's body. The debounced body
          save behind it is complete and takes effect as soon as the item exposes a binding.
        </AlertDescription>
      </Alert>
      <Editor label={documentTitle(doc)} readonly />

      <Dialog open={leavePending && hasUnrevisedEdits}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Unrevised edits</DialogTitle>
            <DialogDescription>
              This document has body edits that no revision covers yet.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            {/* Button intel: "Secondary: Alternative paths, supporting actions". */}
            <Button variant="secondary" onClick={onLeaveCancelled}>
              Keep working
            </Button>
            {/* Button intel: "Primary: Main user goal, maximum 1 per section". */}
            <Button onClick={handleLeaveWithRevise} disabled={saving}>
              Revise
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Container>
  );
}
