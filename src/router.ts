import { createRouter, createWebHashHistory } from "vue-router";
import EmptyView from "./views/EmptyView.vue";

export const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: "/", name: "home", component: EmptyView },
    { path: "/settings/:section?", name: "settings", component: EmptyView },
    { path: "/settings.json", redirect: "/settings/json" },
    { path: "/history", name: "history", component: EmptyView },
    { path: "/changelog", name: "changelog", component: EmptyView },
    { path: "/connection/:id", name: "connection", component: EmptyView },
  ],
});
