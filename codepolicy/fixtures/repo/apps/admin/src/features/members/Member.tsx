import { useQuery, gql } from "@apollo/client";
import create from "zustand";

interface GetMemberQuery {
  member: { id: string };
}

export function Member() {
  const data = fetch("/api/graphql");
  const store = create(() => ({}));
  return { data, store, useQuery, gql };
}
