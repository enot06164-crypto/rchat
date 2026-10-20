import { ChatView } from "./chat/ChatView";
import { Sidebar } from "./sidebar/Sidebar";

/**
 * Панель одного аккаунта: левая колонка (список чатов) и область чата.
 * Панелей может быть две — когда включён сплит-вид.
 */
export function AccountPanel(props: { profile: string }) {
  return (
    <div class="account-panel">
      <Sidebar profile={props.profile} />
      <ChatView profile={props.profile} />
    </div>
  );
}
