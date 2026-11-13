// SPDX-License-Identifier: GPL-3.0-or-later
//
// cad2bem: the minimum-cost perfect matching of Gmsh's triangle-to-quadrilateral recombination without Blossom IV.
//
// Gmsh's Blossom recombination (src/mesh/meshGFaceOptimize.cpp) pairs the triangles of a surface by the perfect
// matching of least cost that Blossom IV (contrib/blossom, Concorde) computes. Blossom IV may be used only within
// Gmsh, which is not compatible with the GPL of a program that links Gmsh, so cad2bem builds Gmsh without it and
// computes the same matching with LEMON (Boost Software License 1.0): MaxWeightedPerfectMatching maximises, and with
// the weight BIG - cost the heaviest perfect matching is the cheapest one. The function takes and gives what
// perfect_match does: the edges (pairs of triangle numbers) and their costs in, the matched edges out as triples
// (first, second, cost) after their count, in arrays allocated with malloc. scripts/build_static_cad.ps1 copies this
// file into the Gmsh sources and calls it in place of perfect_match.

#ifndef CAD2BEM_LEMON_MATCHING_H
#define CAD2BEM_LEMON_MATCHING_H

#define LEMON_ONLY_TEMPLATES
#include <lemon/list_graph.h>
#include <lemon/matching.h>

#include <algorithm>
#include <cstdlib>
#include <tuple>
#include <vector>

// 0 when a perfect matching was found: *elist then holds its edges (count, then first, second, cost per edge).
// Otherwise 1, *elist unchanged. *elen is freed in both cases.
static int cad2bemPerfectMatch(int ncount, int ecount, int **elist, int **elen)
{
  lemon::ListGraph graph;
  std::vector<lemon::ListGraph::Node> nodes;
  nodes.reserve(ncount);
  for(int i = 0; i < ncount; i++) nodes.push_back(graph.addNode());
  lemon::ListGraph::EdgeMap<long long> weight(graph);
  lemon::ListGraph::EdgeMap<int> index(graph);
  // Above every cost Gmsh gives (at most 100000), so that every weight is positive.
  const long long big = 1000000;
  // Gmsh lists the edges in an order that follows memory addresses, and many have equal costs: in that order
  // LEMON would pick another of the cheapest matchings from run to run. Added by cost and triangle numbers, the
  // same mesh gives the same matching.
  std::vector<int> order(ecount);
  for(int k = 0; k < ecount; k++) order[k] = k;
  auto key = [&](int k) {
    const int a = (*elist)[2 * k], b = (*elist)[2 * k + 1];
    return std::make_tuple((*elen)[k], std::min(a, b), std::max(a, b), k);
  };
  std::sort(order.begin(), order.end(), [&](int i, int j) { return key(i) < key(j); });
  for(int k : order) {
    lemon::ListGraph::Edge edge =
      graph.addEdge(nodes[(*elist)[2 * k]], nodes[(*elist)[2 * k + 1]]);
    weight[edge] = big - (*elen)[k];
    index[edge] = k;
  }
  lemon::MaxWeightedPerfectMatching<lemon::ListGraph,
                                    lemon::ListGraph::EdgeMap<long long> >
    matching(graph, weight);
  const bool found = matching.run();
  std::vector<int> matched;
  if(found) {
    for(lemon::ListGraph::EdgeIt edge(graph); edge != lemon::INVALID; ++edge)
      if(matching.matching(edge)) matched.push_back(index[edge]);
    // In the order of the input edges, whatever order the graph keeps them in.
    std::sort(matched.begin(), matched.end());
  }
  int *out = found ? (int *)std::malloc(sizeof(int) * (1 + 3 * matched.size())) : nullptr;
  if(found) {
    out[0] = (int)matched.size();
    for(std::size_t m = 0; m < matched.size(); m++) {
      const int k = matched[m];
      out[1 + 3 * m] = (*elist)[2 * k];
      out[1 + 3 * m + 1] = (*elist)[2 * k + 1];
      out[1 + 3 * m + 2] = (*elen)[k];
    }
    std::free(*elist);
    *elist = out;
  }
  std::free(*elen);
  *elen = nullptr;
  return found ? 0 : 1;
}

#endif
