import { useCallback, useEffect, useState } from "react";
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
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Empty, EmptyDescription, EmptyTitle } from "@/components/ui/empty";
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
    <Container as="section" gap>
      <Container as="header" columns={2}>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
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
        The editor slot. The rafters `editor` item does not compile: it and the
        `document-editor` primitive import editor-history and ops/, which the
        registry publishes under no item name (rafters #2170). Writing those
        modules here would be the hand-authored component the rule forbids, so
        the slot names the gap instead.
      */}
      <Empty>
        <EmptyTitle as="h2">{documentTitle(doc)}</EmptyTitle>
        <EmptyDescription>
          The block editor is not installed. The rafters editor item imports editor-history and
          ops/, which the registry ships under no item name (rafters #2170); the body save and
          revise wiring behind this slot is complete and takes effect as soon as the item does.
        </EmptyDescription>
      </Empty>

      <Dialog open={leavePending && hasUnrevisedEdits} modal>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Unrevised edits</DialogTitle>
            <DialogDescription>
              This document has body edits that no revision covers yet.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={onLeaveCancelled}>
              Keep working
            </Button>
            <Button onClick={handleLeaveWithRevise} disabled={saving}>
              Revise
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Container>
  );
}
