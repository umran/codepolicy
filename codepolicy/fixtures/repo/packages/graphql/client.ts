// The approved GraphQL wrapper is the one place Apollo may be imported directly.
import { ApolloClient, gql } from "@apollo/client";

export const client = new ApolloClient({});
export { gql };
