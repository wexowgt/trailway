import { ObservabilityView } from "@/components/ObservabilityView";

export const metadata = { title: "Observability - Trailway" };

export default async function ObservabilityPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  return <ObservabilityView projectId={id} />;
}
