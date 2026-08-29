import { useState } from "react";
import { Empty, EmptyDescription, EmptyTitle } from "@/components/ui/empty";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import {
  Sidebar,
  SidebarContent,
  SidebarGroup,
  SidebarGroupAction,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuAction,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSub,
  SidebarMenuSubButton,
  SidebarMenuSubItem,
} from "@/components/ui/sidebar";
import type { Document } from "@/lib/documents";
import type { RepoTree, SpecNode, ThesisNode } from "@/lib/tree";
import { documentTitle } from "@/lib/tree";

export interface DocumentTreeProps {
  tree: RepoTree;
  selectedId: string | null;
  onSelect: (doc: Document) => void;
  onCreateIntention: () => void;
}

/**
 * A sidebar row is one line tall and clips what overflows rather than
 * ellipsing it, so legion's sentence-long titles are shortened here and the
 * full title moves to the row's tooltip. Shaping the content, not the styling.
 */
const ROW_LABEL_LIMIT = 30;
const SUB_ROW_LABEL_LIMIT = 26;

function rowLabel(title: string, limit: number): string {
  return title.length > limit ? `${title.slice(0, limit - 1).trimEnd()}...` : title;
}

function SpecRow({
  spec,
  selectedId,
  onSelect,
}: {
  spec: SpecNode;
  selectedId: string | null;
  onSelect: (doc: Document) => void;
}) {
  return (
    <SidebarMenuSubItem>
      <SidebarMenuSubButton
        isActive={spec.document.id === selectedId}
        onClick={() => onSelect(spec.document)}
        title={documentTitle(spec.document)}
      >
        {rowLabel(documentTitle(spec.document), SUB_ROW_LABEL_LIMIT)}
      </SidebarMenuSubButton>
      {spec.workSources.length > 0 && (
        <SidebarMenuSub>
          {spec.workSources.map((link) => (
            <SidebarMenuSubItem key={`${link.kind}-${link.number}`}>
              {/* A work source is a link out, never a document node. */}
              <SidebarMenuSubButton
                href={link.url}
                target="_blank"
                rel="noreferrer"
                size="sm"
                title={link.traceLine}
              >
                {link.kind === "pr" ? `PR #${link.number}` : `Issue #${link.number}`}
              </SidebarMenuSubButton>
            </SidebarMenuSubItem>
          ))}
        </SidebarMenuSub>
      )}
    </SidebarMenuSubItem>
  );
}

function ThesisRow({
  node,
  selectedId,
  onSelect,
}: {
  node: ThesisNode;
  selectedId: string | null;
  onSelect: (doc: Document) => void;
}) {
  return (
    <SidebarMenuItem>
      <SidebarMenuButton
        isActive={node.document.id === selectedId}
        onClick={() => onSelect(node.document)}
        title={node.title}
      >
        {rowLabel(node.title, ROW_LABEL_LIMIT)}
      </SidebarMenuButton>
      {node.specs.length > 0 && (
        <SidebarMenuSub>
          {node.specs.map((spec) => (
            <SpecRow
              key={spec.document.id}
              spec={spec}
              selectedId={selectedId}
              onSelect={onSelect}
            />
          ))}
        </SidebarMenuSub>
      )}
    </SidebarMenuItem>
  );
}

/**
 * The repo tree: `<repo> [cog]` -> an optional Service Design node -> the
 * Intentions group `[+]` -> theses -> their specs -> each spec's work sources.
 */
export function DocumentTree({ tree, selectedId, onSelect, onCreateIntention }: DocumentTreeProps) {
  const [settingsOpen, setSettingsOpen] = useState(false);

  return (
    <Sidebar side="left">
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton size="lg">{tree.repo}</SidebarMenuButton>
            <Sheet open={settingsOpen} onOpenChange={setSettingsOpen}>
              <SheetTrigger asChild>
                <SidebarMenuAction aria-label={`${tree.repo} settings`}>Settings</SidebarMenuAction>
              </SheetTrigger>
              <SheetContent side="left">
                <SheetHeader>
                  <SheetTitle>{tree.repo} settings</SheetTitle>
                  <SheetDescription>Repo-level settings for {tree.repo}.</SheetDescription>
                </SheetHeader>
                <Empty>
                  <EmptyTitle as="h3">Nothing to set yet</EmptyTitle>
                  <EmptyDescription>
                    The settled design names this panel but not its contents, so it ships empty
                    rather than inventing fields.
                  </EmptyDescription>
                </Empty>
              </SheetContent>
            </Sheet>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>

      <SidebarContent>
        {tree.serviceDesignDocs.length > 0 && (
          <SidebarGroup>
            <SidebarGroupLabel>Service Design</SidebarGroupLabel>
            <SidebarGroupContent>
              <SidebarMenu>
                {tree.serviceDesignDocs.map((doc) => (
                  <SidebarMenuItem key={doc.id}>
                    <SidebarMenuButton
                      isActive={doc.id === selectedId}
                      onClick={() => onSelect(doc)}
                      title={documentTitle(doc)}
                    >
                      {rowLabel(documentTitle(doc), ROW_LABEL_LIMIT)}
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                ))}
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>
        )}

        <SidebarGroup>
          <SidebarGroupLabel>Intentions</SidebarGroupLabel>
          <SidebarGroupAction aria-label="New intention" onClick={onCreateIntention}>
            +
          </SidebarGroupAction>
          <SidebarGroupContent>
            <SidebarMenu>
              {tree.intentions.map((node) => (
                <ThesisRow
                  key={node.document.id}
                  node={node}
                  selectedId={selectedId}
                  onSelect={onSelect}
                />
              ))}
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
      </SidebarContent>
    </Sidebar>
  );
}
