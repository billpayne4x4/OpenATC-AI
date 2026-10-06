from pathlib import Path
import sys

source = Path(sys.argv[1])
destination = Path(sys.argv[2])
destination.parent.mkdir(parents=True, exist_ok=True)
with destination.open('w') as output:
    output.write('#pragma once\nnamespace openatc {\n')
    for filename, identifier in [('DejaVuSans.ttf', 'bodyFontData'), ('DejaVuSans-Bold.ttf', 'headingFontData')]:
        data = (source / filename).read_bytes()
        output.write(f'inline const unsigned char {identifier}[] = {{\n')
        for offset in range(0, len(data), 24):
            output.write(','.join(str(value) for value in data[offset:offset + 24]) + ',\n')
        output.write('};\n')
    output.write('}\n')
