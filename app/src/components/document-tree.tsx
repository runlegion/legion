import { useMemo, useState } from "react";
import type { AcceptResult } from "@/hooks/use-documents";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";
import {
  Sidebar,
  SidebarContent,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
} from "@/components/ui/sidebar";
import {
  type Document,
  KIND_GROUP_LABEL,
  KIND_GROUP_ORDER,
  type KindGroup,
  kindGroupFor,
  parsePayload,
  payloadTitle,
  STATUS_FILTER_LABEL,
  STATUS_FILTERS,
  statusBadgeVariant,
  type StatusFilter,
} from "@/lib/documents";

interface TreeRow {
  doc: Document;
  title: string;
}

interface IntentionNode {
  repo: string;
  intention: string;
  allDocuments: Document[];
  visibleGroups: Array<{ group: KindGroup; documents: TreeRow[] }>;
}

interface RepoNode {
  repo: string;
  intentions: IntentionNode[];
}

function buildTree(documents: Document[], statusFilter: StatusFilter): RepoNode[] {
  const byRepo = new Map<string, Map<string, Document[]>>();
  for (const doc of documents) {
    const repo = doc.owner;
    const intention = doc.surface ?? "(no intention)";
    let byIntention = byRepo.get(repo);
    if (!byIntention) {
      byIntention = new Map();
      byRepo.set(repo, byIntention);
    }
    let list = byIntention.get(intention);
    if (!list) {
      list = [];
      byIntention.set(intention, list);
    }
    list.push(doc);
  }

  const repos: RepoNode[] = [];
  for (const [repo, byIntention] of [...byRepo.entries()].sort(([a], [b]) => a.localeCompare(b))) {
    const intentions: IntentionNode[] = [];
    for (const [intention, allDocuments] of [...byIntention.entries()].sort(([a], [b]) =>
      a.localeCompare(b),
    )) {
      const filtered =
        statusFilter === "all"
          ? allDocuments
          : allDocuments.filter((d) => d.status === statusFilter);
      const visibleGroups = KIND_GROUP_ORDER.map((group) => ({
        group,
        documents: filtered
          .filter((d) => kindGroupFor(d.doc_type) === group)
          .sort((a, b) => a.id.localeCompare(b.id))
          .map((doc): TreeRow => ({
            doc,
            title: payloadTitle(parsePayload(doc.payload)) ?? doc.id,
          })),
      })).filter((entry) => entry.documents.length > 0);

      if (visibleGroups.length === 0) continue;
      intentions.push({ repo, intention, allDocuments, visibleGroups });
    }
    if (intentions.length > 0) repos.push({ repo, intentions });
  }

  return repos;
}

function statusCounts(documents: Document[]): {
  draft: number;
  accepted: number;
  published: number;
} {
  let draft = 0;
  let accepted = 0;
  let published = 0;
  for (const doc of documents) {
    if (doc.status === "draft") draft += 1;
    else if (doc.status === "accepted") accepted += 1;
    else if (doc.status === "published") published += 1;
  }
  return { draft, accepted, published };
}

interface DocumentTreeProps {
  documents: Document[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  acceptDocument: (id: string) => Promise<AcceptResult>;
}

export function DocumentTree({
  documents,
  selectedId,
  onSelect,
  acceptDocument,
}: DocumentTreeProps) {
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [bulkProgress, setBulkProgress] = useState<
    Record<string, { done: number; total: number; error: string | null }>
  >({});

  const tree = useMemo(() => buildTree(documents, statusFilter), [documents, statusFilter]);

  const acceptDrafts = async (key: string, draftIds: string[]) => {
    setBulkProgress((prev) => ({
      ...prev,
      [key]: { done: 0, total: draftIds.length, error: null },
    }));
    for (let index = 0; index < draftIds.length; index += 1) {
      const result = await acceptDocument(draftIds[index]);
      if (!result.ok) {
        setBulkProgress((prev) => ({
          ...prev,
          [key]: { done: index, total: draftIds.length, error: result.message },
        }));
        return;
      }
      setBulkProgress((prev) => ({
        ...prev,
        [key]: { done: index + 1, total: draftIds.length, error: null },
      }));
    }
  };

  return (
    <Sidebar>
      <SidebarHeader>
        <label>
          Status
          <Select
            value={statusFilter}
            onValueChange={(value) => setStatusFilter(value as StatusFilter)}
          >
            {STATUS_FILTERS.map((filter) => (
              <option key={filter} value={filter}>
                {STATUS_FILTER_LABEL[filter]}
              </option>
            ))}
          </Select>
        </label>
      </SidebarHeader>

      <SidebarContent>
        {tree.length === 0 && (
          <SidebarGroup>
            <SidebarGroupContent>
              <p>No documents match this filter.</p>
            </SidebarGroupContent>
          </SidebarGroup>
        )}

        {tree.map((repoNode) =>
          repoNode.intentions.map((intentionNode) => {
            const key = `${intentionNode.repo}::${intentionNode.intention}`;
            const counts = statusCounts(intentionNode.allDocuments);
            const draftIds = intentionNode.allDocuments
              .filter((d) => d.status === "draft")
              .map((d) => d.id);
            const progress = bulkProgress[key];

            return (
              <div key={key}>
                <SidebarGroup>
                  <SidebarGroupLabel>
                    {intentionNode.repo} / {intentionNode.intention}
                  </SidebarGroupLabel>
                  <SidebarGroupContent>
                    <p>
                      {counts.draft} draft, {counts.accepted} accepted, {counts.published} published
                    </p>

                    {draftIds.length > 0 && (
                      <>
                        <Button
                          type="button"
                          variant="secondary"
                          size="sm"
                          disabled={
                            progress !== undefined &&
                            progress.done < progress.total &&
                            progress.error === null
                          }
                          onClick={() => void acceptDrafts(key, draftIds)}
                        >
                          Accept {draftIds.length} drafts
                        </Button>
                        {progress && (
                          <p>
                            {progress.error
                              ? `Accepted ${progress.done} of ${progress.total}; failed: ${progress.error}`
                              : `Accepted ${progress.done} of ${progress.total}`}
                          </p>
                        )}
                      </>
                    )}
                  </SidebarGroupContent>
                </SidebarGroup>

                {intentionNode.visibleGroups.map(({ group, documents: groupDocuments }) => (
                  <SidebarGroup key={group}>
                    <SidebarGroupLabel>{KIND_GROUP_LABEL[group]}</SidebarGroupLabel>
                    <SidebarGroupContent>
                      <SidebarMenu>
                        {groupDocuments.map(({ doc, title }) => (
                          <SidebarMenuItem key={doc.id}>
                            <SidebarMenuButton
                              isActive={doc.id === selectedId}
                              onClick={() => onSelect(doc.id)}
                            >
                              {doc.id} {title}
                            </SidebarMenuButton>
                            <SidebarMenuBadge>
                              <Badge variant={statusBadgeVariant(doc.status)}>{doc.status}</Badge>
                            </SidebarMenuBadge>
                          </SidebarMenuItem>
                        ))}
                      </SidebarMenu>
                    </SidebarGroupContent>
                  </SidebarGroup>
                ))}
              </div>
            );
          }),
        )}
      </SidebarContent>
    </Sidebar>
  );
}
