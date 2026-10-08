import { LogsView } from "@/components/LogsView";

export const metadata = { title: "Logs - Trailway" };

export default async function LogsPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  return <LogsView projectId={id} />;
}
