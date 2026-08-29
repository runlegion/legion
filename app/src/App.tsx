import { useEffect, useState } from "react";
import { DocumentReader } from "@/components/document-reader";
import { DocumentTree } from "@/components/document-tree";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import { useDocuments } from "@/hooks/use-documents";

function App() {
  const { documents, error, acceptDocument } = useDocuments();
  const [selectedId, setSelectedId] = useState<string | null>(null);

  useEffect(() => {
    if (selectedId === null && documents.length > 0) {
      setSelectedId(documents[0].id);
    }
  }, [documents, selectedId]);

  const selectedDocument = documents.find((doc) => doc.id === selectedId) ?? null;

  return (
    <SidebarProvider>
      <DocumentTree
        documents={documents}
        selectedId={selectedId}
        onSelect={setSelectedId}
        acceptDocument={acceptDocument}
      />
      <SidebarInset>
        <DocumentReader
          document={selectedDocument}
          fetchError={error}
          acceptDocument={acceptDocument}
        />
      </SidebarInset>
    </SidebarProvider>
  );
}

export default App;
