import { useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import { Container } from "@/components/ui/container";
import { Empty, EmptyDescription, EmptyTitle } from "@/components/ui/empty";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import type { Document } from "@/lib/documents";
import { payloadObject } from "@/lib/tree";

type SettingKind = "string" | "number" | "boolean" | "json";

interface SettingField {
  key: string;
  kind: SettingKind;
  text: string;
}

function kindOf(value: unknown): SettingKind {
  if (typeof value === "string") return "string";
  if (typeof value === "number") return "number";
  if (typeof value === "boolean") return "boolean";
  return "json";
}

function textOf(value: unknown, kind: SettingKind): string {
  if (kind === "json") return JSON.stringify(value, null, 2);
  return String(value);
}

/** Every payload value other than the body, as editable fields. */
function fieldsOf(doc: Document): SettingField[] {
  const payload = payloadObject(doc) ?? {};
  return Object.entries(payload)
    .filter(([key]) => key !== "body")
    .map(([key, value]) => {
      const kind = kindOf(value);
      return { key, kind, text: textOf(value, kind) };
    });
}

/** Convert the edited text back to a payload value, or report why it cannot be. */
function valueOf(field: SettingField): { ok: true; value: unknown } | { ok: false; error: string } {
  switch (field.kind) {
    case "string":
      return { ok: true, value: field.text };
    case "boolean":
      return { ok: true, value: field.text === "true" };
    case "number": {
      const parsed = Number(field.text);
      if (!Number.isFinite(parsed)) return { ok: false, error: "not a number" };
      return { ok: true, value: parsed };
    }
    case "json":
      try {
        return { ok: true, value: JSON.parse(field.text) as unknown };
      } catch {
        return { ok: false, error: "not valid JSON" };
      }
  }
}

export interface SettingsSheetProps {
  document: Document;
  /** Flushes the pending body save, then revises with the merged payload. */
  onSave: (patch: Record<string, unknown>) => Promise<void>;
  saving: boolean;
}

/**
 * The toolbar cog's Sheet: a plain form over every payload key but `body`,
 * saved through the same revise verb as everything else.
 */
export function SettingsSheet({ document: doc, onSave, saving }: SettingsSheetProps) {
  const [open, setOpen] = useState(false);
  const initial = useMemo(() => fieldsOf(doc), [doc]);
  const [fields, setFields] = useState<SettingField[]>(initial);
  const [errors, setErrors] = useState<Record<string, string>>({});

  const handleOpenChange = (next: boolean) => {
    if (next) {
      setFields(fieldsOf(doc));
      setErrors({});
    }
    setOpen(next);
  };

  const edit = (key: string, text: string) => {
    setFields((prev) => prev.map((field) => (field.key === key ? { ...field, text } : field)));
  };

  const handleSave = () => {
    const patch: Record<string, unknown> = {};
    const found: Record<string, string> = {};
    for (const field of fields) {
      const result = valueOf(field);
      if (result.ok) patch[field.key] = result.value;
      else found[field.key] = result.error;
    }
    setErrors(found);
    if (Object.keys(found).length > 0) return;
    void onSave(patch).then(() => setOpen(false));
  };

  return (
    <Sheet open={open} onOpenChange={handleOpenChange}>
      <SheetTrigger asChild>
        {/* Button intel: "ghost/link/outline are the unlimited low-attention
            register" -- the sheet's Save is this panel's one main action. */}
        <Button variant="outline" aria-label="Document settings">
          Settings
        </Button>
      </SheetTrigger>
      {/* Sheet intel: "Choose side based on content relationship
          (left=nav, right=details)" -- these are the document's details. */}
      <SheetContent side="right">
        <SheetHeader>
          <SheetTitle>Settings</SheetTitle>
          <SheetDescription>Every payload value other than the body.</SheetDescription>
        </SheetHeader>

        {fields.length === 0 ? (
          <Empty>
            <EmptyTitle>No other values</EmptyTitle>
            <EmptyDescription>This document's payload holds only a body.</EmptyDescription>
          </Empty>
        ) : (
          <Container as="section">
            {fields.map((field) => (
              <Field key={field.key} label={field.key} error={errors[field.key]}>
                {field.kind === "boolean" ? (
                  <Select value={field.text} onValueChange={(next) => edit(field.key, next)}>
                    <SelectTrigger>
                      <SelectValue placeholder="Choose" />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="true">true</SelectItem>
                      <SelectItem value="false">false</SelectItem>
                    </SelectContent>
                  </Select>
                ) : (
                  <Input
                    value={field.text}
                    onValueChange={(next) => edit(field.key, next)}
                    invalid={Boolean(errors[field.key])}
                  />
                )}
              </Field>
            ))}
          </Container>
        )}

        <SheetFooter>
          {/* Button intel: "Primary: Main user goal, maximum 1 per section". */}
          <Button onClick={handleSave} disabled={saving}>
            {saving ? "Saving..." : "Save"}
          </Button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}
