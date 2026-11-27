#include "kernels/widget.h"

// FIXME: handle empty batches
void Widget::Draw(int width) {
  printf("draw %d\n", width);
}

int RunAll() {
  Widget w;
  w.Draw(10);
  return 0;
}
